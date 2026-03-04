//! WASM algorithm host — custom section based installation and execution runtime.
//!
//! This module implements a strict installation pipeline:
//! `probe -> validate -> persist -> register`.
//! Algorithm metadata is always sourced from the WASM custom section
//! `ng.ai.manifest.v1`.

use crate::decoded::DecodedFrame;
use bytes::Bytes;
use dashmap::DashMap;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{
        AlgorithmInfo, AlgorithmProbeInfo, AlgorithmTestInput, AlgorithmTestResult,
        FrameTransformOutput, ResultClassification, ResultDetection, ResultProcessorOutput,
        WasmAlgorithmManifestV1, WasmExports, WASM_ALGORITHM_MANIFEST_SECTION,
    },
    enums::{ai::AlgorithmModuleType, common::Status},
};
use sha2::Digest;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tracing::{debug, info, warn};
use wasmparser::{Parser, Payload};
use wasmtime::{Config, Engine, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc};

/// A compiled and registered WASM algorithm entry.
struct WasmAlgorithmEntry {
    /// Algorithm metadata.
    info: Arc<AlgorithmInfo>,
    /// Pre-compiled WASM module (shared across invocations).
    module: Arc<Module>,
}

/// Intermediate install payload produced by probe + validation.
struct PreparedInstallArtifact {
    /// Parsed custom section manifest.
    manifest: WasmAlgorithmManifestV1,
    /// SHA-256 checksum (hex lowercase).
    checksum: String,
    /// Artifact size in bytes.
    size: u64,
    /// Compiled module.
    module: Module,
}

/// Borrowed input payload for `FrameTransform` JSON ABI.
#[derive(serde::Serialize)]
struct FrameTransformInputRef<'a> {
    /// Frame width in pixels.
    width: u32,
    /// Frame height in pixels.
    height: u32,
    /// Pointer to RGB pixels in WASM memory.
    pixels_ptr: u32,
    /// Pixel buffer byte length.
    pixels_len: u32,
    /// User-provided configuration object.
    config: &'a serde_json::Value,
}

/// Borrowed input payload for `ResultProcessor` JSON ABI.
#[derive(serde::Serialize)]
struct ResultProcessorInputRef<'a> {
    /// Detection list.
    detections: &'a [ResultDetection],
    /// Classification list.
    classifications: &'a [ResultClassification],
    /// Source frame width.
    frame_width: u32,
    /// Source frame height.
    frame_height: u32,
    /// User-provided configuration object.
    config: &'a serde_json::Value,
}

/// Runtime arguments for one frame transform invocation.
struct FrameTransformRunArgs<'a> {
    /// Raw pixel data (RGB).
    pixel_data: &'a [u8],
    /// Frame width in pixels.
    width: u32,
    /// Frame height in pixels.
    height: u32,
    /// User-provided configuration object.
    config: &'a serde_json::Value,
    /// Fuel budget for this invocation.
    fuel_limit: u64,
    /// Maximum linear memory for this invocation (bytes).
    memory_limit_bytes: usize,
}

/// Runtime arguments for one result processor invocation.
struct ResultProcessorRunArgs<'a> {
    /// Detection list.
    detections: &'a [ResultDetection],
    /// Classification list.
    classifications: &'a [ResultClassification],
    /// Source frame width.
    frame_width: u32,
    /// Source frame height.
    frame_height: u32,
    /// User-provided configuration object.
    config: &'a serde_json::Value,
    /// Fuel budget for this invocation.
    fuel_limit: u64,
    /// Maximum linear memory for this invocation (bytes).
    memory_limit_bytes: usize,
}

/// Per-store resource state used by Wasmtime resource limiting.
struct WasmStoreState {
    limits: StoreLimits,
}

/// WASM algorithm host — manages lifecycle and execution of user-defined algorithms.
pub struct WasmAlgorithmHost {
    /// Wasmtime engine (configured with fuel metering).
    engine: Engine,
    /// Compiled module registry: algorithm_key -> entry.
    modules: DashMap<String, Arc<WasmAlgorithmEntry>>,
    /// Algorithm storage directory.
    algorithms_dir: PathBuf,
    /// Maximum fuel per invocation (prevents runaway algorithms).
    fuel_limit: u64,
    /// Maximum WASM linear memory per instance (bytes).
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

    /// Scan the algorithms directory for `.wasm` files and register valid modules.
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
                            algorithm_key = %info.key,
                            module_type = ?info.module_type,
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

    /// Load and compile a WASM module from an existing file.
    async fn load_from_file(&self, path: &Path) -> Result<Arc<AlgorithmInfo>, AiEngineError> {
        let wasm_bytes = tokio::fs::read(path)
            .await
            .map_err(|e| AiEngineError::IoError(format!("read wasm file: {e}")))?;
        let bytes = Bytes::from(wasm_bytes);
        let prepared = self.prepare_install_artifact(&bytes).await?;
        self.register_prepared_artifact(prepared)
    }

    /// Validate custom section manifest semantic constraints.
    fn validate_manifest(manifest: &WasmAlgorithmManifestV1) -> Result<(), AiEngineError> {
        if manifest.manifest_version != 1 {
            return Err(AiEngineError::AlgorithmError(format!(
                "unsupported manifest version: {}",
                manifest.manifest_version
            )));
        }
        if manifest.algorithm_key.trim().is_empty() {
            return Err(AiEngineError::AlgorithmError(
                "manifest.algorithm_key must not be empty".to_string(),
            ));
        }
        if manifest.name.trim().is_empty() {
            return Err(AiEngineError::AlgorithmError(
                "manifest.name must not be empty".to_string(),
            ));
        }
        Ok(())
    }

    /// Extract and deserialize custom section manifest from a WASM binary.
    fn extract_manifest(wasm_bytes: &[u8]) -> Result<WasmAlgorithmManifestV1, AiEngineError> {
        let mut manifest: Option<WasmAlgorithmManifestV1> = None;

        for payload in Parser::new(0).parse_all(wasm_bytes) {
            let payload = payload
                .map_err(|e| AiEngineError::AlgorithmError(format!("parse wasm payload: {e}")))?;
            if let Payload::CustomSection(section) = payload {
                if section.name() == WASM_ALGORITHM_MANIFEST_SECTION {
                    if manifest.is_some() {
                        return Err(AiEngineError::AlgorithmError(format!(
                            "duplicate custom section '{}'",
                            WASM_ALGORITHM_MANIFEST_SECTION
                        )));
                    }
                    let parsed: WasmAlgorithmManifestV1 = serde_json::from_slice(section.data())
                        .map_err(|e| {
                            AiEngineError::AlgorithmError(format!(
                                "invalid wasm custom section manifest JSON: {e}"
                            ))
                        })?;
                    manifest = Some(parsed);
                }
            }
        }

        manifest.ok_or(AiEngineError::AlgorithmError(format!(
            "missing required custom section '{}'",
            WASM_ALGORITHM_MANIFEST_SECTION
        )))
    }

    /// Compile module and perform full install validation.
    async fn prepare_install_artifact(
        &self,
        wasm_bytes: &Bytes,
    ) -> Result<PreparedInstallArtifact, AiEngineError> {
        let manifest = Self::extract_manifest(wasm_bytes)?;
        Self::validate_manifest(&manifest)?;

        let mut hasher = sha2::Sha256::new();
        hasher.update(wasm_bytes);
        let checksum = hex::encode(hasher.finalize());
        let size = wasm_bytes.len() as u64;

        let engine = self.engine.clone();
        let bytes_for_compile = wasm_bytes.clone();
        let module = tokio::task::spawn_blocking(move || {
            Module::new(&engine, &bytes_for_compile)
                .map_err(|e| AiEngineError::AlgorithmError(format!("wasm compile: {e}")))
        })
        .await
        .map_err(|e| AiEngineError::AlgorithmError(format!("compile task join: {e}")))??;

        self.validate_exports(&module, manifest.module_type)?;

        Ok(PreparedInstallArtifact {
            manifest,
            checksum,
            size,
            module,
        })
    }

    /// Validate that a WASM module has all required exports for its type.
    fn validate_exports(
        &self,
        module: &Module,
        module_type: AlgorithmModuleType,
    ) -> Result<(), AiEngineError> {
        let export_names: Vec<&str> = module.exports().map(|e| e.name()).collect();
        for name in [
            WasmExports::MEMORY,
            WasmExports::ALLOC,
            WasmExports::GET_OUTPUT_LEN,
        ] {
            if !export_names.contains(&name) {
                return Err(AiEngineError::AlgorithmError(format!(
                    "WASM module missing required export: '{name}'"
                )));
            }
        }

        let entry_point = match module_type {
            AlgorithmModuleType::FrameTransform => WasmExports::TRANSFORM,
            AlgorithmModuleType::ResultProcessor => WasmExports::PROCESS,
        };
        if !export_names.contains(&entry_point) {
            return Err(AiEngineError::AlgorithmError(format!(
                "WASM module of type {:?} missing required export: '{}'",
                module_type, entry_point
            )));
        }
        Ok(())
    }

    /// Build a runtime algorithm metadata object from prepared artifact.
    fn build_runtime_info(prepared: &PreparedInstallArtifact) -> AlgorithmInfo {
        AlgorithmInfo {
            id: 0,
            key: prepared.manifest.algorithm_key.clone(),
            name: prepared.manifest.name.clone(),
            description: prepared.manifest.description.clone(),
            version: prepared.manifest.version.clone(),
            module_type: prepared.manifest.module_type,
            path: format!("./ai/algorithms/{}.wasm", prepared.manifest.algorithm_key),
            config_schema: prepared.manifest.config_schema.clone(),
            size: prepared.size,
            status: Status::Enabled,
            checksum: prepared.checksum.clone(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// Register one prepared module into runtime cache.
    fn register_prepared_artifact(
        &self,
        prepared: PreparedInstallArtifact,
    ) -> Result<Arc<AlgorithmInfo>, AiEngineError> {
        if self.modules.contains_key(&prepared.manifest.algorithm_key) {
            return Err(AiEngineError::AlgorithmError(format!(
                "algorithm '{}' already exists",
                prepared.manifest.algorithm_key
            )));
        }

        let info = Arc::new(Self::build_runtime_info(&prepared));
        self.modules.insert(
            info.key.clone(),
            Arc::new(WasmAlgorithmEntry {
                info: Arc::clone(&info),
                module: Arc::new(prepared.module),
            }),
        );
        Ok(info)
    }

    // ── Public control-plane API ──────────────────────────────────

    /// List all registered algorithms.
    pub fn list_algorithms(&self) -> Vec<AlgorithmInfo> {
        self.modules
            .iter()
            .map(|e| e.info.as_ref().clone())
            .collect()
    }

    /// Get one algorithm by algorithm key.
    pub fn get_algorithm(&self, algorithm_key: &str) -> Option<AlgorithmInfo> {
        self.modules
            .get(algorithm_key)
            .map(|e| e.info.as_ref().clone())
    }

    /// Get the count of registered algorithms.
    pub fn algorithm_count(&self) -> usize {
        self.modules.len()
    }

    /// Probe a WASM artifact and return extracted manifest metadata.
    pub async fn probe_algorithm(
        &self,
        wasm_bytes: Bytes,
    ) -> Result<AlgorithmProbeInfo, AiEngineError> {
        let prepared = self.prepare_install_artifact(&wasm_bytes).await?;
        Ok(AlgorithmProbeInfo {
            manifest: prepared.manifest,
            size: prepared.size,
            checksum: prepared.checksum,
        })
    }

    /// Install a WASM artifact using strict install transaction.
    ///
    /// Transaction steps:
    /// 1) probe + validate
    /// 2) persist `.wasm` artifact
    /// 3) register compiled module into runtime
    pub async fn install_algorithm(
        &self,
        wasm_bytes: Bytes,
    ) -> Result<AlgorithmInfo, AiEngineError> {
        let prepared = self.prepare_install_artifact(&wasm_bytes).await?;
        let algorithm_key = prepared.manifest.algorithm_key.clone();
        let wasm_path = self.algorithms_dir.join(format!("{algorithm_key}.wasm"));
        let temp_path = self
            .algorithms_dir
            .join(format!("{algorithm_key}.wasm.installing"));

        if self.modules.contains_key(&algorithm_key) || wasm_path.exists() {
            return Err(AiEngineError::AlgorithmError(format!(
                "algorithm '{}' already exists",
                algorithm_key
            )));
        }

        tokio::fs::write(&temp_path, &wasm_bytes)
            .await
            .map_err(|e| AiEngineError::IoError(format!("write temp wasm file: {e}")))?;
        tokio::fs::rename(&temp_path, &wasm_path)
            .await
            .map_err(|e| AiEngineError::IoError(format!("rename wasm file: {e}")))?;

        let info = match self.register_prepared_artifact(prepared) {
            Ok(info) => info,
            Err(e) => {
                let _ = tokio::fs::remove_file(&wasm_path).await;
                return Err(e);
            }
        };

        info!(
            algorithm_key = %info.key,
            module_type = ?info.module_type,
            size = info.size,
            "WASM algorithm installed and registered"
        );
        Ok(info.as_ref().clone())
    }

    /// Delete a registered algorithm (removes artifact and runtime entry).
    pub async fn delete_algorithm(&self, algorithm_key: &str) -> Result<(), AiEngineError> {
        if self.modules.remove(algorithm_key).is_none() {
            return Err(AiEngineError::AlgorithmError(format!(
                "algorithm '{}' not found",
                algorithm_key
            )));
        }

        let wasm_path = self.algorithms_dir.join(format!("{algorithm_key}.wasm"));
        if wasm_path.exists() {
            tokio::fs::remove_file(&wasm_path)
                .await
                .map_err(|e| AiEngineError::IoError(format!("remove wasm file: {e}")))?;
        }
        info!(algorithm_key, "WASM algorithm deleted");
        Ok(())
    }

    // ── Execution ─────────────────────────────────────────────────

    /// Execute a FrameTransform module with a shared immutable config payload.
    pub async fn execute_frame_transform(
        &self,
        module_id: &str,
        frame: &DecodedFrame,
        config: Arc<serde_json::Value>,
    ) -> Result<DecodedFrame, AiEngineError> {
        let (module, module_type) = {
            let entry = self
                .modules
                .get(module_id)
                .ok_or(AiEngineError::AlgorithmError(format!(
                    "algorithm '{module_id}' not found"
                )))?;
            (Arc::clone(&entry.module), entry.info.module_type)
        };

        if module_type != AlgorithmModuleType::FrameTransform {
            return Err(AiEngineError::AlgorithmError(format!(
                "algorithm '{module_id}' is {:?}, expected FrameTransform",
                module_type
            )));
        }

        let pixel_data = frame.data.clone();
        let width = frame.width;
        let height = frame.height;
        let fuel_limit = self.fuel_limit;
        let memory_limit_bytes = self.memory_limit_bytes;
        let engine = self.engine.clone();

        tokio::task::spawn_blocking(move || {
            let args = FrameTransformRunArgs {
                pixel_data: pixel_data.as_ref(),
                width,
                height,
                config: &config,
                fuel_limit,
                memory_limit_bytes,
            };
            Self::run_frame_transform(&engine, &module, &args)
        })
        .await
        .map_err(|e| AiEngineError::AlgorithmError(format!("frame transform task join: {e}")))?
    }

    /// Execute a ResultProcessor module with a shared immutable config payload.
    pub async fn execute_result_processor(
        &self,
        module_id: &str,
        detections: &[ng_gateway_models::domain::prelude::Detection],
        classifications: &[ng_gateway_models::domain::prelude::Classification],
        frame_width: u32,
        frame_height: u32,
        config: Arc<serde_json::Value>,
    ) -> Result<ResultProcessorOutput, AiEngineError> {
        let (module, module_type) = {
            let entry = self
                .modules
                .get(module_id)
                .ok_or(AiEngineError::AlgorithmError(format!(
                    "algorithm '{module_id}' not found"
                )))?;
            (Arc::clone(&entry.module), entry.info.module_type)
        };

        if module_type != AlgorithmModuleType::ResultProcessor {
            return Err(AiEngineError::AlgorithmError(format!(
                "algorithm '{module_id}' is {:?}, expected ResultProcessor",
                module_type
            )));
        }

        let input_detections: Vec<ResultDetection> =
            detections.iter().map(ResultDetection::from).collect();
        let input_classifications: Vec<ResultClassification> = classifications
            .iter()
            .map(ResultClassification::from)
            .collect();
        let fuel_limit = self.fuel_limit;
        let memory_limit_bytes = self.memory_limit_bytes;
        let engine = self.engine.clone();

        tokio::task::spawn_blocking(move || {
            let run_args = ResultProcessorRunArgs {
                detections: &input_detections,
                classifications: &input_classifications,
                frame_width,
                frame_height,
                config: config.as_ref(),
                fuel_limit,
                memory_limit_bytes,
            };
            Self::run_result_processor(&engine, &module, run_args)
        })
        .await
        .map_err(|e| AiEngineError::AlgorithmError(format!("result processor task join: {e}")))?
    }

    /// Test an algorithm with mock data (for the test API endpoint).
    pub async fn test_algorithm(
        &self,
        algorithm_key: &str,
        test_input: AlgorithmTestInput,
    ) -> Result<AlgorithmTestResult, AiEngineError> {
        let (module, module_type) = {
            let entry = self
                .modules
                .get(algorithm_key)
                .ok_or(AiEngineError::AlgorithmError(format!(
                    "algorithm '{algorithm_key}' not found"
                )))?;
            (Arc::clone(&entry.module), entry.info.module_type)
        };
        let fuel_limit = self.fuel_limit;
        let memory_limit_bytes = self.memory_limit_bytes;
        let engine = self.engine.clone();
        let start = Instant::now();

        let result = tokio::task::spawn_blocking(move || match module_type {
            AlgorithmModuleType::FrameTransform => {
                let pixel_count =
                    test_input.frame_width as usize * test_input.frame_height as usize * 3;
                let dummy_pixels = vec![128u8; pixel_count];
                let args = FrameTransformRunArgs {
                    pixel_data: &dummy_pixels,
                    width: test_input.frame_width,
                    height: test_input.frame_height,
                    config: &test_input.config,
                    fuel_limit,
                    memory_limit_bytes,
                };
                match Self::run_frame_transform(&engine, &module, &args) {
                    Ok(frame) => {
                        let output = ResultProcessorOutput {
                            detections: Vec::new(),
                            classifications: Vec::new(),
                            custom_outputs: vec![(
                                "frame_dimensions".to_string(),
                                serde_json::json!({
                                    "width": frame.width,
                                    "height": frame.height,
                                    "pixel_count": frame.data.len()
                                }),
                            )],
                        };
                        (true, Some(output), None)
                    }
                    Err(e) => (false, None, Some(e.to_string())),
                }
            }
            AlgorithmModuleType::ResultProcessor => {
                let run_args = ResultProcessorRunArgs {
                    detections: &test_input.detections,
                    classifications: &test_input.classifications,
                    frame_width: test_input.frame_width,
                    frame_height: test_input.frame_height,
                    config: &test_input.config,
                    fuel_limit,
                    memory_limit_bytes,
                };
                match Self::run_result_processor(&engine, &module, run_args) {
                    Ok(output) => (true, Some(output), None),
                    Err(e) => (false, None, Some(e.to_string())),
                }
            }
        })
        .await
        .map_err(|e| AiEngineError::AlgorithmError(format!("test task join: {e}")))?;

        let elapsed = start.elapsed();
        let (success, output, error) = result;

        Ok(AlgorithmTestResult {
            success,
            execution_time_ms: elapsed.as_secs_f64() * 1000.0,
            fuel_consumed: fuel_limit,
            output,
            error,
        })
    }

    // ── Internal execution helpers ────────────────────────────────

    /// Run a FrameTransform module synchronously (called from blocking thread).
    fn run_frame_transform(
        engine: &Engine,
        module: &Module,
        args: &FrameTransformRunArgs<'_>,
    ) -> Result<DecodedFrame, AiEngineError> {
        let mut store =
            Self::build_limited_store(engine, args.fuel_limit, args.memory_limit_bytes)?;

        let instance = wasmtime::Instance::new(&mut store, module, &[])
            .map_err(|e| AiEngineError::AlgorithmError(format!("instantiate: {e}")))?;

        let memory = instance
            .get_memory(&mut store, WasmExports::MEMORY)
            .ok_or(AiEngineError::AlgorithmError("no memory export".into()))?;

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
            .call(&mut store, args.pixel_data.len() as i32)
            .map_err(|e| AiEngineError::AlgorithmError(format!("alloc pixels: {e}")))?;

        let mem_data = memory.data_mut(&mut store);
        let pixels_start = pixels_ptr as usize;
        let pixels_end = pixels_start + args.pixel_data.len();
        if pixels_end > mem_data.len() {
            return Err(AiEngineError::AlgorithmError(
                "WASM memory too small for pixel data".into(),
            ));
        }
        mem_data[pixels_start..pixels_end].copy_from_slice(args.pixel_data);

        // 2. Serialize input JSON
        let input = FrameTransformInputRef {
            width: args.width,
            height: args.height,
            pixels_ptr: pixels_ptr as u32,
            pixels_len: args.pixel_data.len() as u32,
            config: args.config,
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
            input_size = args.pixel_data.len(),
            output_size = out_pixels.len(),
            output_w = output.width,
            output_h = output.height,
            "WASM frame transform complete"
        );

        Ok(DecodedFrame {
            data: Bytes::from(out_pixels),
            width: output.width,
            height: output.height,
        })
    }

    /// Run a ResultProcessor module synchronously (called from blocking thread).
    fn run_result_processor(
        engine: &Engine,
        module: &Module,
        args: ResultProcessorRunArgs<'_>,
    ) -> Result<ResultProcessorOutput, AiEngineError> {
        let mut store =
            Self::build_limited_store(engine, args.fuel_limit, args.memory_limit_bytes)?;

        let instance = wasmtime::Instance::new(&mut store, module, &[])
            .map_err(|e| AiEngineError::AlgorithmError(format!("instantiate: {e}")))?;

        let memory = instance
            .get_memory(&mut store, WasmExports::MEMORY)
            .ok_or(AiEngineError::AlgorithmError("no memory export".into()))?;

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
        let input = ResultProcessorInputRef {
            detections: args.detections,
            classifications: args.classifications,
            frame_width: args.frame_width,
            frame_height: args.frame_height,
            config: args.config,
        };
        let input_json = serde_json::to_vec(&input)
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
            input_detections = args.detections.len(),
            output_detections = output.detections.len(),
            custom_outputs = output.custom_outputs.len(),
            "WASM result processor complete"
        );

        Ok(output)
    }

    /// Build one invocation-local store with fuel and linear-memory limits.
    fn build_limited_store(
        engine: &Engine,
        fuel_limit: u64,
        memory_limit_bytes: usize,
    ) -> Result<Store<WasmStoreState>, AiEngineError> {
        let effective_memory_limit = memory_limit_bytes.max(64 * 1024);
        let limits = StoreLimitsBuilder::new()
            .memory_size(effective_memory_limit)
            .build();

        let mut store = Store::new(engine, WasmStoreState { limits });
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(fuel_limit)
            .map_err(|e| AiEngineError::AlgorithmError(format!("set fuel: {e}")))?;
        Ok(store)
    }
}
