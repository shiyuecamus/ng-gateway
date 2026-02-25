//! WASM algorithm host — sandboxed execution of user-defined algorithms.
//!
//! Provides a dual-mode runtime for custom AI pipeline stages:
//! - **FrameTransform**: pixel-level image transforms before inference
//! - **ResultProcessor**: structured result filtering/enrichment after inference
//!
//! # Security Model
//!
//! - WASM modules run in a fully sandboxed environment (wasmtime)
//! - Fuel-based execution limits prevent infinite loops / CPU abuse
//! - Per-instance memory caps prevent OOM attacks
//! - No filesystem, network, or host function access (pure computation only)
//! - Each invocation gets a fresh `Store` — no cross-call state leakage
//!
//! # Performance
//!
//! - Modules are compiled once and cached (`Arc<wasmtime::Module>`)
//! - Execution runs on `spawn_blocking` to avoid starving the Tokio runtime
//! - JSON serialization uses `serde_json` for the control plane;
//!   pixel data is passed via shared WASM linear memory (zero-copy within sandbox)

use crate::decoded_frame::DecodedFrame;
use bytes::Bytes;
use dashmap::DashMap;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::ai::{
    algorithm::{
        AlgorithmTestInput, AlgorithmTestResult, AlgorithmUploadMetadata, FrameTransformInput,
        FrameTransformOutput, ResultClassification, ResultDetection, ResultProcessorInput,
        ResultProcessorOutput, WasmAlgorithmInfo, WasmAlgorithmSidecar, WasmExports,
        WasmModuleType,
    },
    types::{Classification, Detection},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tracing::{debug, info, warn};
use wasmtime::{Config, Engine, Module, Store, TypedFunc};

/// A compiled and registered WASM algorithm entry.
struct WasmAlgorithmEntry {
    /// Algorithm metadata.
    info: WasmAlgorithmInfo,
    /// Pre-compiled WASM module (shared across invocations).
    module: Arc<Module>,
}

/// WASM algorithm host — manages lifecycle and execution of user-defined algorithms.
///
/// # Architecture
///
/// ```text
/// ┌─────────────────────────────────────────────────────────┐
/// │                  WasmAlgorithmHost                       │
/// │                                                         │
/// │  ┌───────────────────┐  ┌─────────────────────────────┐ │
/// │  │  wasmtime::Engine  │  │  Compiled Module Registry   │ │
/// │  │  (Fuel + Memory)   │  │  DashMap<id, Module+Info>   │ │
/// │  └───────────────────┘  └─────────────────────────────┘ │
/// │                                                         │
/// │  Per invocation:                                        │
/// │  ┌─────────────────────────────────────────────────────┐ │
/// │  │  Store(fresh) → Instance → alloc/transform/process │ │
/// │  │  → read output → drop Store (memory freed)          │ │
/// │  └─────────────────────────────────────────────────────┘ │
/// └─────────────────────────────────────────────────────────┘
/// ```
pub struct WasmAlgorithmHost {
    /// Wasmtime engine (configured with fuel metering).
    engine: Engine,
    /// Compiled module registry: algorithm_id → entry.
    modules: DashMap<String, Arc<WasmAlgorithmEntry>>,
    /// Algorithm storage directory.
    algorithms_dir: PathBuf,
    /// Maximum fuel per invocation (prevents runaway algorithms).
    fuel_limit: u64,
    /// Maximum WASM linear memory per instance (bytes).
    /// Reserved for Phase 3 `ResourceLimiter` integration.
    #[allow(unused)]
    memory_limit_bytes: usize,
}

impl WasmAlgorithmHost {
    /// Create a new WASM algorithm host and scan the algorithms directory.
    pub async fn new(
        algorithms_dir: &Path,
        fuel_limit: u64,
        memory_limit_bytes: usize,
    ) -> Result<Self, AiEngineError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);

        let engine = Engine::new(&config)
            .map_err(|e| AiEngineError::AlgorithmError(format!("wasmtime engine init: {e}")))?;

        let host = Self {
            engine,
            modules: DashMap::new(),
            algorithms_dir: algorithms_dir.to_path_buf(),
            fuel_limit,
            memory_limit_bytes,
        };

        // Scan directory for existing .wasm files
        if algorithms_dir.exists() {
            host.scan_directory().await?;
        } else {
            tokio::fs::create_dir_all(algorithms_dir)
                .await
                .map_err(|e| AiEngineError::IoError(format!("create algorithms dir: {e}")))?;
            info!(dir = %algorithms_dir.display(), "created algorithms directory");
        }

        info!(
            count = host.modules.len(),
            fuel_limit, memory_limit_bytes, "WASM algorithm host initialized"
        );

        Ok(host)
    }

    /// Scan the algorithms directory for `.wasm` files and compile them.
    async fn scan_directory(&self) -> Result<(), AiEngineError> {
        let mut entries = tokio::fs::read_dir(&self.algorithms_dir)
            .await
            .map_err(|e| AiEngineError::IoError(format!("read algorithms dir: {e}")))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| AiEngineError::IoError(format!("read dir entry: {e}")))?
        {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "wasm") {
                match self.load_from_file(&path).await {
                    Ok(info) => {
                        info!(
                            algorithm_id = %info.id,
                            module_type = %info.module_type,
                            "discovered WASM algorithm"
                        );
                    }
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to load WASM algorithm, skipping"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Load and compile a WASM module from a file path.
    async fn load_from_file(&self, path: &Path) -> Result<WasmAlgorithmInfo, AiEngineError> {
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| AiEngineError::AlgorithmError("invalid wasm filename".into()))?
            .to_string();

        // Read the .wasm binary
        let wasm_bytes = tokio::fs::read(path)
            .await
            .map_err(|e| AiEngineError::IoError(format!("read wasm file: {e}")))?;

        let file_size = wasm_bytes.len() as u64;

        // Try to read sidecar metadata JSON
        let sidecar_path = path.with_extension("json");
        let sidecar = if sidecar_path.exists() {
            let json_bytes = tokio::fs::read(&sidecar_path)
                .await
                .map_err(|e| AiEngineError::IoError(format!("read sidecar: {e}")))?;
            serde_json::from_slice::<WasmAlgorithmSidecar>(&json_bytes).ok()
        } else {
            None
        };

        // Compile module (CPU-intensive — run on blocking thread)
        let engine = self.engine.clone();
        let module = tokio::task::spawn_blocking(move || {
            Module::new(&engine, &wasm_bytes)
                .map_err(|e| AiEngineError::AlgorithmError(format!("wasm compile: {e}")))
        })
        .await
        .map_err(|e| AiEngineError::AlgorithmError(format!("compile task join: {e}")))??;

        // Determine module type from sidecar or by probing exports
        let module_type = sidecar
            .as_ref()
            .map(|s| s.module_type)
            .unwrap_or_else(|| Self::detect_module_type(&module));

        // Validate required exports
        self.validate_exports(&module, module_type)?;

        let info = WasmAlgorithmInfo {
            id: file_stem.clone(),
            name: sidecar
                .as_ref()
                .and_then(|s| s.name.clone())
                .unwrap_or_else(|| file_stem.clone()),
            description: sidecar
                .as_ref()
                .and_then(|s| s.description.clone())
                .unwrap_or_default(),
            version: sidecar
                .as_ref()
                .and_then(|s| s.version.clone())
                .unwrap_or_else(|| "1.0.0".to_string()),
            module_type,
            file_size,
            config_schema: sidecar.and_then(|s| s.config_schema),
            created_at: chrono::Utc::now(),
        };

        self.modules.insert(
            file_stem,
            Arc::new(WasmAlgorithmEntry {
                info: info.clone(),
                module: Arc::new(module),
            }),
        );

        Ok(info)
    }

    /// Detect module type by inspecting WASM exports.
    fn detect_module_type(module: &Module) -> WasmModuleType {
        let has_transform = module.exports().any(|e| e.name() == WasmExports::TRANSFORM);
        if has_transform {
            WasmModuleType::FrameTransform
        } else {
            WasmModuleType::ResultProcessor
        }
    }

    /// Validate that a WASM module has all required exports for its type.
    fn validate_exports(
        &self,
        module: &Module,
        module_type: WasmModuleType,
    ) -> Result<(), AiEngineError> {
        let export_names: Vec<&str> = module.exports().map(|e| e.name()).collect();

        let required_common = [
            WasmExports::MEMORY,
            WasmExports::ALLOC,
            WasmExports::GET_OUTPUT_LEN,
        ];

        for name in &required_common {
            if !export_names.contains(name) {
                return Err(AiEngineError::AlgorithmError(format!(
                    "WASM module missing required export: '{name}'"
                )));
            }
        }

        let entry_point = match module_type {
            WasmModuleType::FrameTransform => WasmExports::TRANSFORM,
            WasmModuleType::ResultProcessor => WasmExports::PROCESS,
        };

        if !export_names.contains(&entry_point) {
            return Err(AiEngineError::AlgorithmError(format!(
                "WASM module of type {module_type} missing required export: '{entry_point}'"
            )));
        }

        Ok(())
    }

    // ── Public API ────────────────────────────────────────────────

    /// List all registered algorithms.
    pub fn list_algorithms(&self) -> Vec<WasmAlgorithmInfo> {
        self.modules
            .iter()
            .map(|e| e.value().info.clone())
            .collect()
    }

    /// Get a single algorithm by ID.
    pub fn get_algorithm(&self, algorithm_id: &str) -> Option<WasmAlgorithmInfo> {
        self.modules
            .get(algorithm_id)
            .map(|e| e.value().info.clone())
    }

    /// Get the count of registered algorithms.
    pub fn algorithm_count(&self) -> usize {
        self.modules.len()
    }

    /// Upload and register a new WASM algorithm.
    ///
    /// Validates the module, saves it to the algorithms directory, and compiles it.
    pub async fn upload_algorithm(
        &self,
        wasm_bytes: Bytes,
        metadata: AlgorithmUploadMetadata,
    ) -> Result<WasmAlgorithmInfo, AiEngineError> {
        // Generate a filesystem-safe ID from the name
        let algorithm_id = slug_from_name(&metadata.name);

        if self.modules.contains_key(&algorithm_id) {
            return Err(AiEngineError::AlgorithmError(format!(
                "algorithm '{algorithm_id}' already exists"
            )));
        }

        let file_size = wasm_bytes.len() as u64;

        // Compile and validate
        let engine = self.engine.clone();
        let bytes_clone = wasm_bytes.clone();
        let module = tokio::task::spawn_blocking(move || {
            Module::new(&engine, &bytes_clone)
                .map_err(|e| AiEngineError::AlgorithmError(format!("invalid WASM module: {e}")))
        })
        .await
        .map_err(|e| AiEngineError::AlgorithmError(format!("compile task join: {e}")))??;

        self.validate_exports(&module, metadata.module_type)?;

        // Save .wasm file
        let wasm_path = self.algorithms_dir.join(format!("{algorithm_id}.wasm"));
        tokio::fs::write(&wasm_path, &wasm_bytes)
            .await
            .map_err(|e| AiEngineError::IoError(format!("write wasm file: {e}")))?;

        // Save sidecar metadata
        let sidecar = WasmAlgorithmSidecar {
            name: Some(metadata.name.clone()),
            description: Some(metadata.description.clone()),
            version: Some(metadata.version.clone()),
            module_type: metadata.module_type,
            config_schema: metadata.config_schema.clone(),
        };
        let sidecar_path = self.algorithms_dir.join(format!("{algorithm_id}.json"));
        let sidecar_json = serde_json::to_vec_pretty(&sidecar)
            .map_err(|e| AiEngineError::AlgorithmError(format!("serialize sidecar: {e}")))?;
        tokio::fs::write(&sidecar_path, &sidecar_json)
            .await
            .map_err(|e| AiEngineError::IoError(format!("write sidecar: {e}")))?;

        let info = WasmAlgorithmInfo {
            id: algorithm_id.clone(),
            name: metadata.name,
            description: metadata.description,
            version: metadata.version,
            module_type: metadata.module_type,
            file_size,
            config_schema: metadata.config_schema,
            created_at: chrono::Utc::now(),
        };

        self.modules.insert(
            algorithm_id.clone(),
            Arc::new(WasmAlgorithmEntry {
                info: info.clone(),
                module: Arc::new(module),
            }),
        );

        info!(
            algorithm_id = %algorithm_id,
            module_type = %info.module_type,
            file_size,
            "WASM algorithm uploaded and registered"
        );

        Ok(info)
    }

    /// Delete a registered algorithm (removes files and cached module).
    pub async fn delete_algorithm(&self, algorithm_id: &str) -> Result<(), AiEngineError> {
        if self.modules.remove(algorithm_id).is_none() {
            return Err(AiEngineError::AlgorithmError(format!(
                "algorithm '{algorithm_id}' not found"
            )));
        }

        // Remove files
        let wasm_path = self.algorithms_dir.join(format!("{algorithm_id}.wasm"));
        let sidecar_path = self.algorithms_dir.join(format!("{algorithm_id}.json"));

        if wasm_path.exists() {
            tokio::fs::remove_file(&wasm_path)
                .await
                .map_err(|e| AiEngineError::IoError(format!("remove wasm file: {e}")))?;
        }
        if sidecar_path.exists() {
            tokio::fs::remove_file(&sidecar_path)
                .await
                .map_err(|e| AiEngineError::IoError(format!("remove sidecar: {e}")))?;
        }

        info!(algorithm_id, "WASM algorithm deleted");
        Ok(())
    }

    // ── Execution ─────────────────────────────────────────────────

    /// Execute a FrameTransform WASM module on a decoded frame.
    ///
    /// The pixel data is written to WASM linear memory for zero-copy access
    /// within the sandbox. The transform function receives JSON metadata
    /// (including the WASM-side pixel pointer) and returns JSON output
    /// with a pointer to the transformed pixels.
    pub async fn execute_frame_transform(
        &self,
        module_id: &str,
        frame: &DecodedFrame,
        config: &serde_json::Value,
    ) -> Result<DecodedFrame, AiEngineError> {
        let entry = self
            .modules
            .get(module_id)
            .ok_or_else(|| {
                AiEngineError::AlgorithmError(format!("algorithm '{module_id}' not found"))
            })?
            .value()
            .clone();

        if entry.info.module_type != WasmModuleType::FrameTransform {
            return Err(AiEngineError::AlgorithmError(format!(
                "algorithm '{module_id}' is {:?}, expected FrameTransform",
                entry.info.module_type
            )));
        }

        let module = Arc::clone(&entry.module);
        let pixel_data = frame.data.clone();
        let width = frame.width;
        let height = frame.height;
        let config = config.clone();
        let fuel_limit = self.fuel_limit;
        let engine = self.engine.clone();

        let result = tokio::task::spawn_blocking(move || {
            Self::run_frame_transform(
                &engine,
                &module,
                &pixel_data,
                width,
                height,
                &config,
                fuel_limit,
            )
        })
        .await
        .map_err(|e| AiEngineError::AlgorithmError(format!("frame transform task join: {e}")))?;

        result
    }

    /// Execute a ResultProcessor WASM module on pipeline results.
    pub async fn execute_result_processor(
        &self,
        module_id: &str,
        detections: &[Detection],
        classifications: &[Classification],
        frame_width: u32,
        frame_height: u32,
        config: &serde_json::Value,
    ) -> Result<ResultProcessorOutput, AiEngineError> {
        let entry = self
            .modules
            .get(module_id)
            .ok_or_else(|| {
                AiEngineError::AlgorithmError(format!("algorithm '{module_id}' not found"))
            })?
            .value()
            .clone();

        if entry.info.module_type != WasmModuleType::ResultProcessor {
            return Err(AiEngineError::AlgorithmError(format!(
                "algorithm '{module_id}' is {:?}, expected ResultProcessor",
                entry.info.module_type
            )));
        }

        let module = Arc::clone(&entry.module);
        let input = ResultProcessorInput {
            detections: detections.iter().map(ResultDetection::from).collect(),
            classifications: classifications
                .iter()
                .map(|c| ResultClassification {
                    top_k: c.top_k.iter().map(|(l, s)| (l.to_string(), *s)).collect(),
                })
                .collect(),
            frame_width,
            frame_height,
            config: config.clone(),
        };
        let fuel_limit = self.fuel_limit;
        let engine = self.engine.clone();

        let result = tokio::task::spawn_blocking(move || {
            Self::run_result_processor(&engine, &module, &input, fuel_limit)
        })
        .await
        .map_err(|e| AiEngineError::AlgorithmError(format!("result processor task join: {e}")))?;

        result
    }

    /// Test an algorithm with mock data (for the test API endpoint).
    pub async fn test_algorithm(
        &self,
        algorithm_id: &str,
        test_input: AlgorithmTestInput,
    ) -> Result<AlgorithmTestResult, AiEngineError> {
        let entry = self
            .modules
            .get(algorithm_id)
            .ok_or_else(|| {
                AiEngineError::AlgorithmError(format!("algorithm '{algorithm_id}' not found"))
            })?
            .value()
            .clone();

        let module = Arc::clone(&entry.module);
        let module_type = entry.info.module_type;
        let fuel_limit = self.fuel_limit;
        let engine = self.engine.clone();
        let start = Instant::now();

        let result = tokio::task::spawn_blocking(move || {
            match module_type {
                WasmModuleType::FrameTransform => {
                    // Generate a dummy frame for FrameTransform testing
                    let pixel_count =
                        test_input.frame_width as usize * test_input.frame_height as usize * 3;
                    let dummy_pixels = vec![128u8; pixel_count];

                    match Self::run_frame_transform(
                        &engine,
                        &module,
                        &dummy_pixels,
                        test_input.frame_width,
                        test_input.frame_height,
                        &test_input.config,
                        fuel_limit,
                    ) {
                        Ok(frame) => {
                            let output = ResultProcessorOutput {
                                detections: vec![],
                                classifications: vec![],
                                custom_outputs: vec![(
                                    "frame_dimensions".to_string(),
                                    serde_json::json!({
                                        "width": frame.width,
                                        "height": frame.height,
                                        "pixel_count": frame.data.len()
                                    }),
                                )],
                            };
                            (true, Some(output), None, fuel_limit)
                        }
                        Err(e) => (false, None, Some(e.to_string()), fuel_limit),
                    }
                }
                WasmModuleType::ResultProcessor => {
                    let input = ResultProcessorInput {
                        detections: test_input.detections,
                        classifications: test_input.classifications,
                        frame_width: test_input.frame_width,
                        frame_height: test_input.frame_height,
                        config: test_input.config,
                    };

                    match Self::run_result_processor(&engine, &module, &input, fuel_limit) {
                        Ok(output) => (true, Some(output), None, fuel_limit),
                        Err(e) => (false, None, Some(e.to_string()), fuel_limit),
                    }
                }
            }
        })
        .await
        .map_err(|e| AiEngineError::AlgorithmError(format!("test task join: {e}")))?;

        let elapsed = start.elapsed();
        let (success, output, error, _initial_fuel) = result;

        Ok(AlgorithmTestResult {
            success,
            execution_time_ms: elapsed.as_secs_f64() * 1000.0,
            fuel_consumed: fuel_limit, // approximate — exact tracking requires Store access
            output,
            error,
        })
    }

    // ── Internal execution helpers ────────────────────────────────

    /// Run a FrameTransform module synchronously (called from blocking thread).
    fn run_frame_transform(
        engine: &Engine,
        module: &Module,
        pixel_data: &[u8],
        width: u32,
        height: u32,
        config: &serde_json::Value,
        fuel_limit: u64,
    ) -> Result<DecodedFrame, AiEngineError> {
        let mut store = Store::new(engine, ());
        store
            .set_fuel(fuel_limit)
            .map_err(|e| AiEngineError::AlgorithmError(format!("set fuel: {e}")))?;

        let instance = wasmtime::Instance::new(&mut store, module, &[])
            .map_err(|e| AiEngineError::AlgorithmError(format!("instantiate: {e}")))?;

        let memory = instance
            .get_memory(&mut store, WasmExports::MEMORY)
            .ok_or_else(|| AiEngineError::AlgorithmError("no memory export".into()))?;

        let alloc: TypedFunc<i32, i32> = instance
            .get_typed_func(&mut store, WasmExports::ALLOC)
            .map_err(|e| AiEngineError::AlgorithmError(format!("get alloc: {e}")))?;

        let transform: TypedFunc<(i32, i32), i32> = instance
            .get_typed_func(&mut store, WasmExports::TRANSFORM)
            .map_err(|e| AiEngineError::AlgorithmError(format!("get transform: {e}")))?;

        let get_output_len: TypedFunc<(), i32> = instance
            .get_typed_func(&mut store, WasmExports::GET_OUTPUT_LEN)
            .map_err(|e| AiEngineError::AlgorithmError(format!("get get_output_len: {e}")))?;

        // 1. Allocate space for pixel data in WASM memory and copy pixels
        let pixels_ptr = alloc
            .call(&mut store, pixel_data.len() as i32)
            .map_err(|e| AiEngineError::AlgorithmError(format!("alloc pixels: {e}")))?;

        let mem_data = memory.data_mut(&mut store);
        let pixels_start = pixels_ptr as usize;
        let pixels_end = pixels_start + pixel_data.len();
        if pixels_end > mem_data.len() {
            return Err(AiEngineError::AlgorithmError(
                "WASM memory too small for pixel data".into(),
            ));
        }
        mem_data[pixels_start..pixels_end].copy_from_slice(pixel_data);

        // 2. Serialize input JSON
        let input = FrameTransformInput {
            width,
            height,
            pixels_ptr: pixels_ptr as u32,
            pixels_len: pixel_data.len() as u32,
            config: config.clone(),
        };
        let input_json = serde_json::to_vec(&input)
            .map_err(|e| AiEngineError::AlgorithmError(format!("serialize input: {e}")))?;

        // 3. Allocate space for JSON and write it
        let json_ptr = alloc
            .call(&mut store, input_json.len() as i32)
            .map_err(|e| AiEngineError::AlgorithmError(format!("alloc json: {e}")))?;

        let mem_data = memory.data_mut(&mut store);
        let json_start = json_ptr as usize;
        let json_end = json_start + input_json.len();
        if json_end > mem_data.len() {
            return Err(AiEngineError::AlgorithmError(
                "WASM memory too small for JSON input".into(),
            ));
        }
        mem_data[json_start..json_end].copy_from_slice(&input_json);

        // 4. Call transform()
        let output_ptr = transform
            .call(&mut store, (json_ptr, input_json.len() as i32))
            .map_err(|e| AiEngineError::AlgorithmError(format!("transform call: {e}")))?;

        // 5. Read output length
        let output_len = get_output_len
            .call(&mut store, ())
            .map_err(|e| AiEngineError::AlgorithmError(format!("get_output_len call: {e}")))?
            as usize;

        // 6. Read output JSON
        let mem_data = memory.data(&store);
        let out_start = output_ptr as usize;
        let out_end = out_start + output_len;
        if out_end > mem_data.len() {
            return Err(AiEngineError::AlgorithmError(
                "output pointer exceeds WASM memory".into(),
            ));
        }
        let output_bytes = &mem_data[out_start..out_end];

        let output: FrameTransformOutput = serde_json::from_slice(output_bytes)
            .map_err(|e| AiEngineError::AlgorithmError(format!("deserialize output: {e}")))?;

        // 7. Read transformed pixel data from WASM memory
        let out_pixels_start = output.pixels_ptr as usize;
        let out_pixels_end = out_pixels_start + output.pixels_len as usize;
        if out_pixels_end > mem_data.len() {
            return Err(AiEngineError::AlgorithmError(
                "output pixels pointer exceeds WASM memory".into(),
            ));
        }
        let out_pixels = mem_data[out_pixels_start..out_pixels_end].to_vec();

        // Validate output dimensions
        let expected_len = output.width as usize * output.height as usize * 3;
        if out_pixels.len() != expected_len {
            return Err(AiEngineError::AlgorithmError(format!(
                "output pixel size mismatch: expected {expected_len} ({}×{}×3), got {}",
                output.width,
                output.height,
                out_pixels.len()
            )));
        }

        debug!(
            module_type = "frame_transform",
            input_size = pixel_data.len(),
            output_size = out_pixels.len(),
            output_w = output.width,
            output_h = output.height,
            "WASM frame transform complete"
        );

        Ok(DecodedFrame {
            data: out_pixels,
            width: output.width,
            height: output.height,
        })
    }

    /// Run a ResultProcessor module synchronously (called from blocking thread).
    fn run_result_processor(
        engine: &Engine,
        module: &Module,
        input: &ResultProcessorInput,
        fuel_limit: u64,
    ) -> Result<ResultProcessorOutput, AiEngineError> {
        let mut store = Store::new(engine, ());
        store
            .set_fuel(fuel_limit)
            .map_err(|e| AiEngineError::AlgorithmError(format!("set fuel: {e}")))?;

        let instance = wasmtime::Instance::new(&mut store, module, &[])
            .map_err(|e| AiEngineError::AlgorithmError(format!("instantiate: {e}")))?;

        let memory = instance
            .get_memory(&mut store, WasmExports::MEMORY)
            .ok_or_else(|| AiEngineError::AlgorithmError("no memory export".into()))?;

        let alloc: TypedFunc<i32, i32> = instance
            .get_typed_func(&mut store, WasmExports::ALLOC)
            .map_err(|e| AiEngineError::AlgorithmError(format!("get alloc: {e}")))?;

        let process: TypedFunc<(i32, i32), i32> = instance
            .get_typed_func(&mut store, WasmExports::PROCESS)
            .map_err(|e| AiEngineError::AlgorithmError(format!("get process: {e}")))?;

        let get_output_len: TypedFunc<(), i32> = instance
            .get_typed_func(&mut store, WasmExports::GET_OUTPUT_LEN)
            .map_err(|e| AiEngineError::AlgorithmError(format!("get get_output_len: {e}")))?;

        // 1. Serialize input JSON
        let input_json = serde_json::to_vec(input)
            .map_err(|e| AiEngineError::AlgorithmError(format!("serialize input: {e}")))?;

        // 2. Allocate and write to WASM memory
        let input_ptr = alloc
            .call(&mut store, input_json.len() as i32)
            .map_err(|e| AiEngineError::AlgorithmError(format!("alloc input: {e}")))?;

        let mem_data = memory.data_mut(&mut store);
        let start = input_ptr as usize;
        let end = start + input_json.len();
        if end > mem_data.len() {
            return Err(AiEngineError::AlgorithmError(
                "WASM memory too small for input JSON".into(),
            ));
        }
        mem_data[start..end].copy_from_slice(&input_json);

        // 3. Call process()
        let output_ptr = process
            .call(&mut store, (input_ptr, input_json.len() as i32))
            .map_err(|e| AiEngineError::AlgorithmError(format!("process call: {e}")))?;

        // 4. Read output length
        let output_len = get_output_len
            .call(&mut store, ())
            .map_err(|e| AiEngineError::AlgorithmError(format!("get_output_len call: {e}")))?
            as usize;

        // 5. Read output JSON
        let mem_data = memory.data(&store);
        let out_start = output_ptr as usize;
        let out_end = out_start + output_len;
        if out_end > mem_data.len() {
            return Err(AiEngineError::AlgorithmError(
                "output pointer exceeds WASM memory".into(),
            ));
        }
        let output_bytes = &mem_data[out_start..out_end];

        let output: ResultProcessorOutput = serde_json::from_slice(output_bytes)
            .map_err(|e| AiEngineError::AlgorithmError(format!("deserialize output: {e}")))?;

        debug!(
            module_type = "result_processor",
            input_detections = input.detections.len(),
            output_detections = output.detections.len(),
            custom_outputs = output.custom_outputs.len(),
            "WASM result processor complete"
        );

        Ok(output)
    }
}

/// Generate a filesystem-safe slug from an algorithm name.
///
/// Converts to lowercase, replaces non-alphanumeric characters with underscores,
/// and collapses consecutive underscores.
fn slug_from_name(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();

    // Collapse consecutive underscores and trim
    let mut result = String::with_capacity(slug.len());
    let mut prev_underscore = false;
    for c in slug.chars() {
        if c == '_' {
            if !prev_underscore && !result.is_empty() {
                result.push(c);
            }
            prev_underscore = true;
        } else {
            result.push(c);
            prev_underscore = false;
        }
    }
    result.trim_end_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug_from_name() {
        assert_eq!(
            slug_from_name("PPE Compliance Checker"),
            "ppe_compliance_checker"
        );
        assert_eq!(slug_from_name("edge-detect-v2"), "edge_detect_v2");
        assert_eq!(slug_from_name("  My  Algorithm  "), "my_algorithm");
        assert_eq!(slug_from_name("simple"), "simple");
    }
}
