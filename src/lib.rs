mod channel;
mod client;
mod server;
use std::sync::OnceLock;
use pyo3::prelude::*;

use crate::channel::{ReceiverHandle, SenderHandle};
use crate::client::connect;
use crate::server::serve;




pub fn get_taunicorn_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();

    VERSION.get_or_init(|| {
        let version = env!("CARGO_PKG_VERSION");
        version.replace("-alpha", "a").replace("-beta", "b")
    })
}

#[pymodule(gil_used = false)]
fn _taunicorn(_py: Python, module: &Bound<PyModule>) -> PyResult<()> {
    module.add("__version__", get_taunicorn_version())?;
    module.add_class::<SenderHandle>()?;
    module.add_class::<ReceiverHandle>()?;
    module.add_function(wrap_pyfunction!(connect, module)?)?;
    module.add_function(wrap_pyfunction!(serve, module)?)?;
    Ok(())
}
