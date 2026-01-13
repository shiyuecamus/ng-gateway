/// Sanitize a string component for stable OPC UA String NodeId usage.
///
/// We keep a conservative allow-list to maximize interoperability:
/// - `[A-Za-z0-9._-]` are kept
/// - everything else becomes `-`
///
/// IMPORTANT: Changing this policy after release breaks client-side mappings.
pub fn sanitize_nodeid_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out
}

/// Build the NG-Gateway NodeId string (without `ns=1;` prefix).
///
/// Format: `{channel}.{device}.{point_key}`
pub fn make_nodeid_path(channel: &str, device: &str, point_key: &str) -> String {
    format!(
        "{}.{}.{}",
        sanitize_nodeid_component(channel),
        sanitize_nodeid_component(device),
        sanitize_nodeid_component(point_key)
    )
}
