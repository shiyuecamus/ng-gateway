/// MC protocol stack root module.
///
/// Submodules under this namespace define frame structures, codec functions,
/// session management and planning logic for the Mitsubishi MC protocol.
pub mod codec;
pub mod error;
pub mod frame;
pub mod planner;
pub mod session;
pub mod types;
