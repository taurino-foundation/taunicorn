use once_cell::sync::OnceCell;
use pyo3::prelude::*;
use std::sync::OnceLock;
use tokio::runtime::{Builder, Runtime};

use crate::{
    channel::{PyBroadcastChannel, PyBroadcastReceiver, PyChannel},
    client::Connector,
    server::Acceptor,
};
mod channel;
mod client;
mod server;
/* mod utils; */

static TOKIO_RUNTIME: OnceCell<Runtime> = OnceCell::new();

pub fn get_taunicorn_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();

    VERSION.get_or_init(|| {
        let version = env!("CARGO_PKG_VERSION");
        version.replace("-alpha", "a").replace("-beta", "b")
    })
}

#[pymodule(gil_used = false)]
fn _taunicorn(_py: Python, module: &Bound<PyModule>) -> PyResult<()> {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build Tokio runtime");

    // nur einmal setzen
    let runtime = TOKIO_RUNTIME.get_or_init(|| runtime);

    pyo3_async_runtimes::tokio::init_with_runtime(runtime).map_err(|_| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("Tokio runtime already initialized")
    })?;

    module.add("__version__", get_taunicorn_version())?;
    module.add_class::<Connector>()?;
    module.add_class::<Acceptor>()?;
    module.add_class::<PyChannel>()?;
    module.add_class::<PyBroadcastChannel>()?;
    module.add_class::<PyBroadcastReceiver>()?;
    Ok(())
}
