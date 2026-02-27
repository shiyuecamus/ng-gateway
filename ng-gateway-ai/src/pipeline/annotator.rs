//! Frame annotation — draws detection/classification results onto frames.
//!
//! Produces annotated JPEG images for live preview, alarm snapshots, and debug.
//! Phase 1 uses an embedded 8×13 bitmap font for text rendering (no external
//! font files required). Phase 3 will upgrade to `ab_glyph` for anti-aliased
//! TrueType rendering.

#[cfg(feature = "engine")]
mod inner {
    use crate::decoded::DecodedFrame;
    use bytes::Bytes;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::ai::{
        pipeline::AnnotationConfig,
        types::{AnalysisCore, Detection, KeypointDetection, SegmentationMask},
    };
    use rayon::prelude::*;

    /// Frame annotator trait — draws analysis results onto frames.
    pub trait FrameAnnotator: Send + Sync + 'static {
        /// Draw analysis results onto a decoded frame and encode as JPEG.
        fn annotate(
            &self,
            frame: &DecodedFrame,
            result: &AnalysisCore,
            config: &AnnotationConfig,
        ) -> Result<Bytes, AiEngineError>;
    }

    /// Default frame annotator using the `image` crate for drawing.
    ///
    /// Draws bounding boxes, class labels, confidence scores, and tracking IDs
    /// onto frames. Uses an embedded 8×13 monospace bitmap font for text
    /// rendering — zero external dependencies, suitable for headless/embedded
    /// gateway deployments.
    pub struct DefaultFrameAnnotator;

    impl FrameAnnotator for DefaultFrameAnnotator {
        fn annotate(
            &self,
            frame: &DecodedFrame,
            result: &AnalysisCore,
            config: &AnnotationConfig,
        ) -> Result<Bytes, AiEngineError> {
            let mut img =
                image::RgbImage::from_raw(frame.width, frame.height, frame.data.as_ref().to_vec())
                    .ok_or(AiEngineError::InternalError("invalid frame data".into()))?;
            let compiled_palette = compile_palette(&config.color_palette);

            // Draw segmentation mask overlay (semi-transparent)
            if config.draw_segmentation {
                for mask in result.segmentation_masks.iter() {
                    draw_segmentation_overlay(
                        &mut img,
                        mask,
                        &compiled_palette,
                        config.segmentation_alpha,
                        config.segmentation_background_class,
                    );
                }
            }

            if config.draw_bboxes {
                // Draw object detections
                for (i, det) in result.detections.iter().enumerate() {
                    let color = palette_color(&compiled_palette, i);

                    let x1 = (det.bbox.x_min * frame.width as f32) as i32;
                    let y1 = (det.bbox.y_min * frame.height as f32) as i32;
                    let x2 = (det.bbox.x_max * frame.width as f32) as i32;
                    let y2 = (det.bbox.y_max * frame.height as f32) as i32;

                    draw_rect(&mut img, x1, y1, x2, y2, color, config.line_thickness);

                    let label = build_label(config, det);
                    if !label.is_empty() {
                        let scale = config.font_scale;
                        draw_label_with_bg(&mut img, x1, y1, &label, color, scale);
                    }
                }

                // Draw keypoint/pose detections
                for (i, kpd) in result.keypoint_detections.iter().enumerate() {
                    let color = palette_color(&compiled_palette, i);

                    let x1 = (kpd.bbox.x_min * frame.width as f32) as i32;
                    let y1 = (kpd.bbox.y_min * frame.height as f32) as i32;
                    let x2 = (kpd.bbox.x_max * frame.width as f32) as i32;
                    let y2 = (kpd.bbox.y_max * frame.height as f32) as i32;

                    draw_rect(&mut img, x1, y1, x2, y2, color, config.line_thickness);

                    if config.draw_labels || config.draw_confidence {
                        let mut parts = Vec::with_capacity(3);
                        if config.draw_labels {
                            parts.push(kpd.class.to_string());
                        }
                        if config.draw_confidence {
                            parts.push(format!("{:.0}%", kpd.confidence * 100.0));
                        }
                        if config.draw_track_ids {
                            if let Some(tid) = kpd.track_id {
                                parts.push(format!("#{tid}"));
                            }
                        }
                        let label = parts.join(" ");
                        if !label.is_empty() {
                            draw_label_with_bg(&mut img, x1, y1, &label, color, config.font_scale);
                        }
                    }

                    // Draw keypoints and skeleton connections
                    draw_keypoints(&mut img, kpd, frame.width, frame.height, color);
                }
            }

            // Optionally downscale for bandwidth savings
            let final_img = if let Some(max_dim) = config.max_output_dimension {
                if frame.width > max_dim || frame.height > max_dim {
                    let scale = max_dim as f32 / frame.width.max(frame.height) as f32;
                    let nw = (frame.width as f32 * scale) as u32;
                    let nh = (frame.height as f32 * scale) as u32;
                    image::DynamicImage::ImageRgb8(image::imageops::resize(
                        &img,
                        nw,
                        nh,
                        image::imageops::FilterType::Triangle,
                    ))
                    .to_rgb8()
                } else {
                    img
                }
            } else {
                img
            };

            // Encode to JPEG
            let mut jpeg_buf = Vec::with_capacity(frame.width as usize * frame.height as usize / 4);
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut jpeg_buf,
                config.jpeg_quality,
            );
            image::DynamicImage::ImageRgb8(final_img)
                .write_with_encoder(encoder)
                .map_err(|e| AiEngineError::InternalError(e.to_string()))?;

            Ok(Bytes::from(jpeg_buf))
        }
    }

    /// Build the annotation label from detection metadata according to config.
    fn build_label(config: &AnnotationConfig, det: &Detection) -> String {
        let mut parts = Vec::with_capacity(3);

        if config.draw_labels {
            parts.push(det.class.to_string());
        }
        if config.draw_confidence {
            parts.push(format!("{:.0}%", det.confidence * 100.0));
        }
        if config.draw_track_ids {
            if let Some(tid) = det.track_id {
                parts.push(format!("#{tid}"));
            }
        }

        parts.join(" ")
    }

    /// Parse a hex color string (e.g., "#FF3838") to `[R, G, B]`.
    fn parse_palette_color(palette: &[String], idx: usize) -> [u8; 3] {
        palette
            .get(idx)
            .and_then(|hex| {
                let hex = hex.trim_start_matches('#');
                if hex.len() != 6 {
                    return None;
                }
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some([r, g, b])
            })
            .unwrap_or([255, 56, 56])
    }

    /// Compile the runtime palette once per annotation call.
    ///
    /// Parsing hex strings up-front removes string processing from the hot per-pixel path.
    fn compile_palette(palette: &[String]) -> Vec<[u8; 3]> {
        if palette.is_empty() {
            return vec![[255, 56, 56]];
        }
        (0..palette.len())
            .map(|idx| parse_palette_color(palette, idx))
            .collect()
    }

    /// Return a palette color with safe fallback and cyclic indexing.
    #[inline]
    fn palette_color(compiled_palette: &[[u8; 3]], idx: usize) -> [u8; 3] {
        let len = compiled_palette.len().max(1);
        compiled_palette
            .get(idx % len)
            .copied()
            .unwrap_or([255, 56, 56])
    }

    /// Draw a rectangle outline on an RGB image with configurable thickness.
    fn draw_rect(
        img: &mut image::RgbImage,
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        color: [u8; 3],
        thickness: u32,
    ) {
        let w = img.width() as i32;
        let h = img.height() as i32;
        let pixel = image::Rgb(color);
        let t = thickness as i32;

        for d in 0..t {
            // Top edge
            for x in x1..=x2 {
                let y = y1 + d;
                if x >= 0 && x < w && y >= 0 && y < h {
                    img.put_pixel(x as u32, y as u32, pixel);
                }
            }
            // Bottom edge
            for x in x1..=x2 {
                let y = y2 - d;
                if x >= 0 && x < w && y >= 0 && y < h {
                    img.put_pixel(x as u32, y as u32, pixel);
                }
            }
            // Left edge
            for y in y1..=y2 {
                let x = x1 + d;
                if x >= 0 && x < w && y >= 0 && y < h {
                    img.put_pixel(x as u32, y as u32, pixel);
                }
            }
            // Right edge
            for y in y1..=y2 {
                let x = x2 - d;
                if x >= 0 && x < w && y >= 0 && y < h {
                    img.put_pixel(x as u32, y as u32, pixel);
                }
            }
        }
    }

    /// Draw a text label with a filled color background at the given anchor point.
    ///
    /// The label is rendered above the bounding box (y offset upward). The
    /// background rectangle uses the bbox color; text is drawn in contrasting
    /// white or black depending on the background luminance.
    fn draw_label_with_bg(
        img: &mut image::RgbImage,
        x: i32,
        y: i32,
        text: &str,
        bg_color: [u8; 3],
        font_scale: f32,
    ) {
        let char_w = (GLYPH_WIDTH as f32 * font_scale).round() as i32;
        let char_h = (GLYPH_HEIGHT as f32 * font_scale).round() as i32;
        let padding = 2i32;

        let text_w = char_w * text.len() as i32;
        let box_w = text_w + padding * 2;
        let box_h = char_h + padding * 2;

        // Position the label background just above the bbox top edge
        let bg_y = y - box_h;
        let bg_x = x;

        let img_w = img.width() as i32;
        let img_h = img.height() as i32;

        // Fill background rectangle
        let bg_pixel = image::Rgb(bg_color);
        for py in bg_y.max(0)..((bg_y + box_h).min(img_h)) {
            for px in bg_x.max(0)..((bg_x + box_w).min(img_w)) {
                img.put_pixel(px as u32, py as u32, bg_pixel);
            }
        }

        // Choose text color for contrast (Rec. 601 luminance)
        let lum =
            0.299 * bg_color[0] as f32 + 0.587 * bg_color[1] as f32 + 0.114 * bg_color[2] as f32;
        let text_color = if lum > 128.0 {
            [0u8, 0, 0]
        } else {
            [255u8, 255, 255]
        };

        // Draw each character using the embedded bitmap font
        let text_x = bg_x + padding;
        let text_y = bg_y + padding;
        for (ci, ch) in text.chars().enumerate() {
            draw_bitmap_char(
                img,
                text_x + ci as i32 * char_w,
                text_y,
                ch,
                text_color,
                font_scale,
            );
        }
    }

    /// Draw a single character using the embedded bitmap font with scaling.
    fn draw_bitmap_char(
        img: &mut image::RgbImage,
        ox: i32,
        oy: i32,
        ch: char,
        color: [u8; 3],
        scale: f32,
    ) {
        let glyph = bitmap_glyph(ch);
        let img_w = img.width() as i32;
        let img_h = img.height() as i32;
        let pixel = image::Rgb(color);
        let scaled_w = (GLYPH_WIDTH as f32 * scale).round() as i32;
        let scaled_h = (GLYPH_HEIGHT as f32 * scale).round() as i32;

        for sy in 0..scaled_h {
            for sx in 0..scaled_w {
                // Map scaled coordinate back to glyph bitmap coordinate
                let gx = (sx as f32 / scale) as usize;
                let gy = (sy as f32 / scale) as usize;
                if gy < GLYPH_HEIGHT
                    && gx < GLYPH_WIDTH
                    && glyph[gy] & (1 << (GLYPH_WIDTH - 1 - gx)) != 0
                {
                    let px = ox + sx;
                    let py = oy + sy;
                    if px >= 0 && px < img_w && py >= 0 && py < img_h {
                        img.put_pixel(px as u32, py as u32, pixel);
                    }
                }
            }
        }
    }

    // ── Keypoint drawing ──────────────────────────────────────────────

    /// COCO 17-keypoint skeleton connectivity (pairs of keypoint indices).
    const COCO_SKELETON: [(usize, usize); 19] = [
        (0, 1),
        (0, 2),
        (1, 3),
        (2, 4),
        (5, 6),
        (5, 7),
        (7, 9),
        (6, 8),
        (8, 10),
        (5, 11),
        (6, 12),
        (11, 12),
        (11, 13),
        (13, 15),
        (12, 14),
        (14, 16),
        (0, 5),
        (0, 6),
        (3, 5),
    ];

    /// Minimum keypoint confidence to draw.
    const KP_CONF_THRESHOLD: f32 = 0.3;

    /// Draw keypoints and skeleton connections for a pose detection.
    fn draw_keypoints(
        img: &mut image::RgbImage,
        kpd: &KeypointDetection,
        frame_w: u32,
        frame_h: u32,
        color: [u8; 3],
    ) {
        let kps = &kpd.keypoints;

        // Draw skeleton lines first (behind dots)
        let limb_pairs = if kps.len() == 17 {
            &COCO_SKELETON[..]
        } else {
            &[] as &[(usize, usize)]
        };

        for &(a, b) in limb_pairs {
            if a >= kps.len() || b >= kps.len() {
                continue;
            }
            if kps[a].confidence < KP_CONF_THRESHOLD || kps[b].confidence < KP_CONF_THRESHOLD {
                continue;
            }
            let x1 = (kps[a].x * frame_w as f32) as i32;
            let y1 = (kps[a].y * frame_h as f32) as i32;
            let x2 = (kps[b].x * frame_w as f32) as i32;
            let y2 = (kps[b].y * frame_h as f32) as i32;
            draw_line(img, x1, y1, x2, y2, color);
        }

        // Draw keypoint dots
        let dot_color = [255u8, 255, 255];
        for kp in kps {
            if kp.confidence < KP_CONF_THRESHOLD {
                continue;
            }
            let px = (kp.x * frame_w as f32) as i32;
            let py = (kp.y * frame_h as f32) as i32;
            draw_circle(img, px, py, 3, dot_color);
        }
    }

    /// Draw a line between two points using Bresenham's algorithm.
    fn draw_line(img: &mut image::RgbImage, x0: i32, y0: i32, x1: i32, y1: i32, color: [u8; 3]) {
        let w = img.width() as i32;
        let h = img.height() as i32;
        let pixel = image::Rgb(color);

        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut cx = x0;
        let mut cy = y0;

        loop {
            if cx >= 0 && cx < w && cy >= 0 && cy < h {
                img.put_pixel(cx as u32, cy as u32, pixel);
            }
            if cx == x1 && cy == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                cx += sx;
            }
            if e2 <= dx {
                err += dx;
                cy += sy;
            }
        }
    }

    /// Draw a filled circle at the given center with the given radius.
    fn draw_circle(img: &mut image::RgbImage, cx: i32, cy: i32, radius: i32, color: [u8; 3]) {
        let w = img.width() as i32;
        let h = img.height() as i32;
        let pixel = image::Rgb(color);
        let r2 = radius * radius;

        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy <= r2 {
                    let px = cx + dx;
                    let py = cy + dy;
                    if px >= 0 && px < w && py >= 0 && py < h {
                        img.put_pixel(px as u32, py as u32, pixel);
                    }
                }
            }
        }
    }

    // ── Segmentation overlay ────────────────────────────────────────

    /// Draw a semi-transparent segmentation mask overlay on the frame.
    ///
    /// Each class index maps to a color from the palette. The overlay
    /// alpha is fixed at 40% to preserve underlying image detail.
    fn draw_segmentation_overlay(
        img: &mut image::RgbImage,
        mask: &SegmentationMask,
        compiled_palette: &[[u8; 3]],
        alpha: f32,
        background_class: Option<u8>,
    ) {
        let img_w = img.width() as usize;
        let img_h = img.height() as usize;
        let mask_w = mask.width as usize;
        let mask_h = mask.height as usize;

        if img_w == 0 || img_h == 0 || mask_w == 0 || mask_h == 0 {
            return;
        }

        if mask.mask.len() != mask_w * mask_h {
            return;
        }

        let alpha_fp = (alpha.clamp(0.0, 1.0) * 256.0).round() as u16;
        if alpha_fp == 0 {
            return;
        }
        let inv_alpha_fp = 256u16.saturating_sub(alpha_fp);
        let palette_len = compiled_palette.len().max(1);
        let row_stride = img_w * 3;

        img.as_mut()
            .par_chunks_mut(row_stride)
            .enumerate()
            .for_each(|(y, row)| {
                for x in 0..img_w {
                    let mx = (x * mask_w) / img_w;
                    let my = (y * mask_h) / img_h;
                    let class_idx = mask.mask[my * mask_w + mx] as usize;

                    if let Some(bg) = background_class {
                        if class_idx == bg as usize {
                            continue;
                        }
                    }

                    let overlay = compiled_palette[class_idx % palette_len];
                    let px = x * 3;
                    let r =
                        ((row[px] as u16 * inv_alpha_fp + overlay[0] as u16 * alpha_fp) >> 8) as u8;
                    let g = ((row[px + 1] as u16 * inv_alpha_fp + overlay[1] as u16 * alpha_fp)
                        >> 8) as u8;
                    let b = ((row[px + 2] as u16 * inv_alpha_fp + overlay[2] as u16 * alpha_fp)
                        >> 8) as u8;
                    row[px] = r;
                    row[px + 1] = g;
                    row[px + 2] = b;
                }
            });
    }

    // ── Embedded bitmap font ──────────────────────────────────────────
    //
    // Compact 6×10 monospace bitmap font covering ASCII 0x20–0x7E.
    // Each glyph is represented as 10 rows of u8 bitmasks (top 6 bits used).

    const GLYPH_WIDTH: usize = 6;
    const GLYPH_HEIGHT: usize = 10;

    /// Retrieve the bitmap glyph for an ASCII character.
    /// Non-printable / non-ASCII characters fall back to a solid block.
    fn bitmap_glyph(ch: char) -> &'static [u8; GLYPH_HEIGHT] {
        let idx = ch as usize;
        if (0x20..=0x7E).contains(&idx) {
            &FONT_6X10[idx - 0x20]
        } else {
            &FONT_6X10[0] // space fallback
        }
    }

    /// 6×10 bitmap font table for ASCII 0x20 (' ') through 0x7E ('~').
    ///
    /// Each row is a u8 where the top 6 bits (bits 7–2) represent pixels
    /// left-to-right. Bit 7 is the leftmost pixel, bit 2 is the rightmost.
    #[rustfmt::skip]
    static FONT_6X10: [[u8; GLYPH_HEIGHT]; 95] = [
        // 0x20 ' ' (space)
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        // 0x21 '!'
        [0x00, 0x20, 0x20, 0x20, 0x20, 0x20, 0x00, 0x20, 0x00, 0x00],
        // 0x22 '"'
        [0x00, 0x28, 0x28, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        // 0x23 '#'
        [0x00, 0x28, 0x7C, 0x28, 0x28, 0x7C, 0x28, 0x00, 0x00, 0x00],
        // 0x24 '$'
        [0x00, 0x20, 0x3C, 0x60, 0x38, 0x0C, 0x78, 0x20, 0x00, 0x00],
        // 0x25 '%'
        [0x00, 0x64, 0x68, 0x08, 0x10, 0x20, 0x2C, 0x4C, 0x00, 0x00],
        // 0x26 '&'
        [0x00, 0x30, 0x48, 0x30, 0x50, 0x4C, 0x44, 0x3C, 0x00, 0x00],
        // 0x27 '\''
        [0x00, 0x20, 0x20, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        // 0x28 '('
        [0x00, 0x10, 0x20, 0x20, 0x20, 0x20, 0x20, 0x10, 0x00, 0x00],
        // 0x29 ')'
        [0x00, 0x20, 0x10, 0x10, 0x10, 0x10, 0x10, 0x20, 0x00, 0x00],
        // 0x2A '*'
        [0x00, 0x00, 0x28, 0x10, 0x7C, 0x10, 0x28, 0x00, 0x00, 0x00],
        // 0x2B '+'
        [0x00, 0x00, 0x10, 0x10, 0x7C, 0x10, 0x10, 0x00, 0x00, 0x00],
        // 0x2C ','
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x20, 0x40, 0x00],
        // 0x2D '-'
        [0x00, 0x00, 0x00, 0x00, 0x7C, 0x00, 0x00, 0x00, 0x00, 0x00],
        // 0x2E '.'
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00],
        // 0x2F '/'
        [0x00, 0x04, 0x08, 0x08, 0x10, 0x20, 0x20, 0x40, 0x00, 0x00],
        // 0x30 '0'
        [0x00, 0x38, 0x44, 0x4C, 0x54, 0x64, 0x44, 0x38, 0x00, 0x00],
        // 0x31 '1'
        [0x00, 0x10, 0x30, 0x10, 0x10, 0x10, 0x10, 0x38, 0x00, 0x00],
        // 0x32 '2'
        [0x00, 0x38, 0x44, 0x04, 0x08, 0x10, 0x20, 0x7C, 0x00, 0x00],
        // 0x33 '3'
        [0x00, 0x38, 0x44, 0x04, 0x18, 0x04, 0x44, 0x38, 0x00, 0x00],
        // 0x34 '4'
        [0x00, 0x08, 0x18, 0x28, 0x48, 0x7C, 0x08, 0x08, 0x00, 0x00],
        // 0x35 '5'
        [0x00, 0x7C, 0x40, 0x78, 0x04, 0x04, 0x44, 0x38, 0x00, 0x00],
        // 0x36 '6'
        [0x00, 0x18, 0x20, 0x40, 0x78, 0x44, 0x44, 0x38, 0x00, 0x00],
        // 0x37 '7'
        [0x00, 0x7C, 0x04, 0x08, 0x10, 0x20, 0x20, 0x20, 0x00, 0x00],
        // 0x38 '8'
        [0x00, 0x38, 0x44, 0x44, 0x38, 0x44, 0x44, 0x38, 0x00, 0x00],
        // 0x39 '9'
        [0x00, 0x38, 0x44, 0x44, 0x3C, 0x04, 0x08, 0x30, 0x00, 0x00],
        // 0x3A ':'
        [0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00],
        // 0x3B ';'
        [0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x20, 0x20, 0x40, 0x00],
        // 0x3C '<'
        [0x00, 0x08, 0x10, 0x20, 0x40, 0x20, 0x10, 0x08, 0x00, 0x00],
        // 0x3D '='
        [0x00, 0x00, 0x00, 0x7C, 0x00, 0x7C, 0x00, 0x00, 0x00, 0x00],
        // 0x3E '>'
        [0x00, 0x20, 0x10, 0x08, 0x04, 0x08, 0x10, 0x20, 0x00, 0x00],
        // 0x3F '?'
        [0x00, 0x38, 0x44, 0x04, 0x08, 0x10, 0x00, 0x10, 0x00, 0x00],
        // 0x40 '@'
        [0x00, 0x38, 0x44, 0x5C, 0x54, 0x5C, 0x40, 0x38, 0x00, 0x00],
        // 0x41 'A'
        [0x00, 0x38, 0x44, 0x44, 0x7C, 0x44, 0x44, 0x44, 0x00, 0x00],
        // 0x42 'B'
        [0x00, 0x78, 0x44, 0x44, 0x78, 0x44, 0x44, 0x78, 0x00, 0x00],
        // 0x43 'C'
        [0x00, 0x38, 0x44, 0x40, 0x40, 0x40, 0x44, 0x38, 0x00, 0x00],
        // 0x44 'D'
        [0x00, 0x78, 0x44, 0x44, 0x44, 0x44, 0x44, 0x78, 0x00, 0x00],
        // 0x45 'E'
        [0x00, 0x7C, 0x40, 0x40, 0x78, 0x40, 0x40, 0x7C, 0x00, 0x00],
        // 0x46 'F'
        [0x00, 0x7C, 0x40, 0x40, 0x78, 0x40, 0x40, 0x40, 0x00, 0x00],
        // 0x47 'G'
        [0x00, 0x38, 0x44, 0x40, 0x5C, 0x44, 0x44, 0x38, 0x00, 0x00],
        // 0x48 'H'
        [0x00, 0x44, 0x44, 0x44, 0x7C, 0x44, 0x44, 0x44, 0x00, 0x00],
        // 0x49 'I'
        [0x00, 0x38, 0x10, 0x10, 0x10, 0x10, 0x10, 0x38, 0x00, 0x00],
        // 0x4A 'J'
        [0x00, 0x1C, 0x08, 0x08, 0x08, 0x08, 0x48, 0x30, 0x00, 0x00],
        // 0x4B 'K'
        [0x00, 0x44, 0x48, 0x50, 0x60, 0x50, 0x48, 0x44, 0x00, 0x00],
        // 0x4C 'L'
        [0x00, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x7C, 0x00, 0x00],
        // 0x4D 'M'
        [0x00, 0x44, 0x6C, 0x54, 0x44, 0x44, 0x44, 0x44, 0x00, 0x00],
        // 0x4E 'N'
        [0x00, 0x44, 0x64, 0x54, 0x4C, 0x44, 0x44, 0x44, 0x00, 0x00],
        // 0x4F 'O'
        [0x00, 0x38, 0x44, 0x44, 0x44, 0x44, 0x44, 0x38, 0x00, 0x00],
        // 0x50 'P'
        [0x00, 0x78, 0x44, 0x44, 0x78, 0x40, 0x40, 0x40, 0x00, 0x00],
        // 0x51 'Q'
        [0x00, 0x38, 0x44, 0x44, 0x44, 0x54, 0x48, 0x34, 0x00, 0x00],
        // 0x52 'R'
        [0x00, 0x78, 0x44, 0x44, 0x78, 0x50, 0x48, 0x44, 0x00, 0x00],
        // 0x53 'S'
        [0x00, 0x38, 0x44, 0x40, 0x38, 0x04, 0x44, 0x38, 0x00, 0x00],
        // 0x54 'T'
        [0x00, 0x7C, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x00, 0x00],
        // 0x55 'U'
        [0x00, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x38, 0x00, 0x00],
        // 0x56 'V'
        [0x00, 0x44, 0x44, 0x44, 0x44, 0x28, 0x28, 0x10, 0x00, 0x00],
        // 0x57 'W'
        [0x00, 0x44, 0x44, 0x44, 0x54, 0x54, 0x6C, 0x44, 0x00, 0x00],
        // 0x58 'X'
        [0x00, 0x44, 0x44, 0x28, 0x10, 0x28, 0x44, 0x44, 0x00, 0x00],
        // 0x59 'Y'
        [0x00, 0x44, 0x44, 0x28, 0x10, 0x10, 0x10, 0x10, 0x00, 0x00],
        // 0x5A 'Z'
        [0x00, 0x7C, 0x04, 0x08, 0x10, 0x20, 0x40, 0x7C, 0x00, 0x00],
        // 0x5B '['
        [0x00, 0x38, 0x20, 0x20, 0x20, 0x20, 0x20, 0x38, 0x00, 0x00],
        // 0x5C '\'
        [0x00, 0x40, 0x20, 0x20, 0x10, 0x08, 0x08, 0x04, 0x00, 0x00],
        // 0x5D ']'
        [0x00, 0x38, 0x08, 0x08, 0x08, 0x08, 0x08, 0x38, 0x00, 0x00],
        // 0x5E '^'
        [0x00, 0x10, 0x28, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        // 0x5F '_'
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7C, 0x00, 0x00],
        // 0x60 '`'
        [0x00, 0x20, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        // 0x61 'a'
        [0x00, 0x00, 0x00, 0x38, 0x04, 0x3C, 0x44, 0x3C, 0x00, 0x00],
        // 0x62 'b'
        [0x00, 0x40, 0x40, 0x78, 0x44, 0x44, 0x44, 0x78, 0x00, 0x00],
        // 0x63 'c'
        [0x00, 0x00, 0x00, 0x38, 0x44, 0x40, 0x44, 0x38, 0x00, 0x00],
        // 0x64 'd'
        [0x00, 0x04, 0x04, 0x3C, 0x44, 0x44, 0x44, 0x3C, 0x00, 0x00],
        // 0x65 'e'
        [0x00, 0x00, 0x00, 0x38, 0x44, 0x7C, 0x40, 0x38, 0x00, 0x00],
        // 0x66 'f'
        [0x00, 0x18, 0x24, 0x20, 0x78, 0x20, 0x20, 0x20, 0x00, 0x00],
        // 0x67 'g'
        [0x00, 0x00, 0x00, 0x3C, 0x44, 0x44, 0x3C, 0x04, 0x38, 0x00],
        // 0x68 'h'
        [0x00, 0x40, 0x40, 0x78, 0x44, 0x44, 0x44, 0x44, 0x00, 0x00],
        // 0x69 'i'
        [0x00, 0x10, 0x00, 0x30, 0x10, 0x10, 0x10, 0x38, 0x00, 0x00],
        // 0x6A 'j'
        [0x00, 0x08, 0x00, 0x18, 0x08, 0x08, 0x08, 0x48, 0x30, 0x00],
        // 0x6B 'k'
        [0x00, 0x40, 0x40, 0x48, 0x50, 0x60, 0x50, 0x48, 0x00, 0x00],
        // 0x6C 'l'
        [0x00, 0x30, 0x10, 0x10, 0x10, 0x10, 0x10, 0x38, 0x00, 0x00],
        // 0x6D 'm'
        [0x00, 0x00, 0x00, 0x68, 0x54, 0x54, 0x44, 0x44, 0x00, 0x00],
        // 0x6E 'n'
        [0x00, 0x00, 0x00, 0x78, 0x44, 0x44, 0x44, 0x44, 0x00, 0x00],
        // 0x6F 'o'
        [0x00, 0x00, 0x00, 0x38, 0x44, 0x44, 0x44, 0x38, 0x00, 0x00],
        // 0x70 'p'
        [0x00, 0x00, 0x00, 0x78, 0x44, 0x44, 0x78, 0x40, 0x40, 0x00],
        // 0x71 'q'
        [0x00, 0x00, 0x00, 0x3C, 0x44, 0x44, 0x3C, 0x04, 0x04, 0x00],
        // 0x72 'r'
        [0x00, 0x00, 0x00, 0x58, 0x64, 0x40, 0x40, 0x40, 0x00, 0x00],
        // 0x73 's'
        [0x00, 0x00, 0x00, 0x3C, 0x40, 0x38, 0x04, 0x78, 0x00, 0x00],
        // 0x74 't'
        [0x00, 0x20, 0x20, 0x78, 0x20, 0x20, 0x24, 0x18, 0x00, 0x00],
        // 0x75 'u'
        [0x00, 0x00, 0x00, 0x44, 0x44, 0x44, 0x44, 0x3C, 0x00, 0x00],
        // 0x76 'v'
        [0x00, 0x00, 0x00, 0x44, 0x44, 0x44, 0x28, 0x10, 0x00, 0x00],
        // 0x77 'w'
        [0x00, 0x00, 0x00, 0x44, 0x44, 0x54, 0x6C, 0x44, 0x00, 0x00],
        // 0x78 'x'
        [0x00, 0x00, 0x00, 0x44, 0x28, 0x10, 0x28, 0x44, 0x00, 0x00],
        // 0x79 'y'
        [0x00, 0x00, 0x00, 0x44, 0x44, 0x3C, 0x04, 0x04, 0x38, 0x00],
        // 0x7A 'z'
        [0x00, 0x00, 0x00, 0x7C, 0x08, 0x10, 0x20, 0x7C, 0x00, 0x00],
        // 0x7B '{'
        [0x00, 0x0C, 0x10, 0x10, 0x60, 0x10, 0x10, 0x0C, 0x00, 0x00],
        // 0x7C '|'
        [0x00, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x00, 0x00],
        // 0x7D '}'
        [0x00, 0x60, 0x10, 0x10, 0x0C, 0x10, 0x10, 0x60, 0x00, 0x00],
        // 0x7E '~'
        [0x00, 0x00, 0x00, 0x24, 0x58, 0x00, 0x00, 0x00, 0x00, 0x00],
    ];

    #[cfg(test)]
    mod tests {
        use super::*;
        use ng_gateway_models::ai::types::{
            AnalysisCore, BoundingBox, Detection, SegmentationMask,
        };

        fn make_test_frame(w: u32, h: u32) -> DecodedFrame {
            DecodedFrame {
                data: Bytes::from(vec![128u8; (w * h * 3) as usize]),
                width: w,
                height: h,
            }
        }

        #[test]
        fn annotate_empty_detections() {
            let annotator = DefaultFrameAnnotator;
            let frame = make_test_frame(320, 240);
            let result = AnalysisCore::default();
            let config = AnnotationConfig::default();
            let out = annotator.annotate(&frame, &result, &config).unwrap();
            assert!(!out.is_empty());
        }

        #[test]
        fn annotate_with_detections() {
            let annotator = DefaultFrameAnnotator;
            let frame = make_test_frame(640, 480);
            let result = AnalysisCore {
                detections: vec![
                    Detection {
                        bbox: BoundingBox {
                            x_min: 0.1,
                            y_min: 0.2,
                            x_max: 0.5,
                            y_max: 0.6,
                        },
                        class: "person".into(),
                        class_id: 0,
                        confidence: 0.87,
                        track_id: Some(42),
                    },
                    Detection {
                        bbox: BoundingBox {
                            x_min: 0.6,
                            y_min: 0.3,
                            x_max: 0.9,
                            y_max: 0.8,
                        },
                        class: "car".into(),
                        class_id: 1,
                        confidence: 0.65,
                        track_id: None,
                    },
                ]
                .into(),
                ..Default::default()
            };
            let config = AnnotationConfig::default();
            let out = annotator.annotate(&frame, &result, &config).unwrap();
            assert!(!out.is_empty());
        }

        #[test]
        fn annotate_respects_config_flags() {
            let annotator = DefaultFrameAnnotator;
            let frame = make_test_frame(320, 240);
            let result = AnalysisCore {
                detections: vec![Detection {
                    bbox: BoundingBox {
                        x_min: 0.1,
                        y_min: 0.2,
                        x_max: 0.5,
                        y_max: 0.6,
                    },
                    class: "dog".into(),
                    class_id: 16,
                    confidence: 0.99,
                    track_id: Some(1),
                }]
                .into(),
                ..Default::default()
            };
            let config = AnnotationConfig {
                draw_bboxes: true,
                draw_labels: false,
                draw_confidence: false,
                draw_track_ids: false,
                ..Default::default()
            };
            let out = annotator.annotate(&frame, &result, &config).unwrap();
            assert!(!out.is_empty());
        }

        #[test]
        fn label_builder_formats_correctly() {
            let config = AnnotationConfig::default();
            let det = Detection {
                bbox: BoundingBox {
                    x_min: 0.0,
                    y_min: 0.0,
                    x_max: 1.0,
                    y_max: 1.0,
                },
                class: "person".into(),
                class_id: 0,
                confidence: 0.873,
                track_id: Some(5),
            };
            let label = build_label(&config, &det);
            assert!(label.contains("person"));
            assert!(label.contains("87%"));
            assert!(label.contains("#5"));
        }

        #[test]
        fn parse_palette_color_valid() {
            let palette = vec!["#FF0000".into(), "#00FF00".into()];
            assert_eq!(parse_palette_color(&palette, 0), [255, 0, 0]);
            assert_eq!(parse_palette_color(&palette, 1), [0, 255, 0]);
        }

        #[test]
        fn parse_palette_color_fallback() {
            let palette: Vec<String> = vec!["invalid".into()];
            assert_eq!(parse_palette_color(&palette, 0), [255, 56, 56]);
        }

        #[test]
        fn segmentation_alpha_zero_keeps_image_unchanged() {
            let annotator = DefaultFrameAnnotator;
            let frame = make_test_frame(64, 64);

            let baseline = annotator
                .annotate(
                    &frame,
                    &AnalysisCore::default(),
                    &AnnotationConfig::default(),
                )
                .unwrap();

            let with_mask = AnalysisCore {
                segmentation_masks: vec![SegmentationMask {
                    mask: vec![1u8; 16 * 16],
                    width: 16,
                    height: 16,
                    labels: vec!["bg".into(), "obj".into()],
                }]
                .into(),
                ..Default::default()
            };
            let cfg = AnnotationConfig {
                draw_segmentation: true,
                segmentation_alpha: 0.0,
                segmentation_background_class: None,
                ..Default::default()
            };
            let output = annotator.annotate(&frame, &with_mask, &cfg).unwrap();

            assert_eq!(
                baseline, output,
                "alpha=0 should not alter the rendered frame"
            );
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
