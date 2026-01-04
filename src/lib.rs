use pyo3::prelude::*;
use std::sync::OnceLock;

use crate::{
    channel::{PyBroadcastChannel, PyBroadcastReceiver, PyChannel},
    client::Connector,
    server::Acceptor,
};
mod channel;
mod client;
mod server;
/* mod utils; */


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
    module.add_class::<Connector>()?;
    module.add_class::<Acceptor>()?;
    module.add_class::<PyChannel>()?;
    module.add_class::<PyBroadcastChannel>()?;
    module.add_class::<PyBroadcastReceiver>()?;
    Ok(())
}
