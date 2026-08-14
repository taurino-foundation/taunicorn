// pyo3 version 0.27

use interprocess::local_socket::Name;
use interprocess::local_socket::tokio::{RecvHalf, SendHalf};
use interprocess::local_socket::{GenericFilePath, ToFsName as _};
use interprocess::local_socket::{
    ListenerOptions,
    traits::tokio::{Listener as _, Stream as _},
};
#[cfg(windows)]
use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
#[cfg(windows)]
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::{path::PathBuf, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
#[cfg(windows)]
use widestring::U16CString;

#[pyclass]
pub struct Server {
    name: Name<'static>,
    reader: Arc<Mutex<Option<RecvHalf>>>,
    writer: Arc<Mutex<Option<SendHalf>>>,
    shutdown: CancellationToken,

    #[cfg(unix)]
    mode: Option<libc::mode_t>,

    #[cfg(windows)]
    sddl: Option<String>,
}


#[pymethods]
impl Server {
#[new]
#[pyo3(signature = (name, *, mode=None, sddl=None))]
pub fn new(
    name: String,
    mode: Option<u32>,
    sddl: Option<String>,
) -> PyResult<Self> {
    #[cfg(windows)]
    let _=mode;
    let path = format!(r"\\.\pipe\{}", name);
    #[cfg(unix)]
    let path = format!("/tmp/{}", name);

    let name = PathBuf::from(path)
        .to_fs_name::<GenericFilePath>()?
        .into_owned();

    Ok(Self {
        name,
        reader: Arc::new(Mutex::new(None)),
        writer: Arc::new(Mutex::new(None)),
        shutdown: CancellationToken::new(),

        #[cfg(unix)]
        mode: mode.map(|m| m as libc::mode_t),

        #[cfg(windows)]
        sddl,
    })
}


    pub fn send<'py>(
        &self,
        py: Python<'py>,
        data: Py<PyBytes>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let payload: Vec<u8> = data.bind(py).as_bytes().to_vec();
        // 🔹 2. Arc klonen (niemals self capturen!)
        let writer = self.writer.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = writer.lock().await;

            if let Some(w) = guard.as_mut() {
                w.write_all(&payload)
                    .await
                    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
                Ok(())
            } else {
                Err(PyRuntimeError::new_err("No client connected"))
            }
        })
    }

    pub fn receive<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let reader = self.reader.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // 1) async: bytes lesen (nur Rust-Daten, Send)
            let mut guard = reader.lock().await;
            let r = guard.as_mut().ok_or_else(|| {
                PyRuntimeError::new_err("No client connected")
            })?;

            let mut buf = vec![0u8; 4096];
            let n = r
                .read(&mut buf)
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

            if n == 0 {
                return Err(PyRuntimeError::new_err("Client disconnected"));
            }
            buf.truncate(n);

            // 2) jetzt: Python-Objekt erzeugen, aber als Py<PyBytes> (Send)
            let pybytes: Py<PyBytes> =
                Python::attach(|py| PyBytes::new(py, &buf).unbind());

            // future_into_py erwartet PyResult<T> wo T in Python konvertierbar ist
            Ok(pybytes)
        })
    }

    pub fn serve<'py>(
        &mut self,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let name = self.name.clone();
        let shutdown = self.shutdown.clone();
        let reader = self.reader.clone();
        let writer = self.writer.clone();

        let mut opts = ListenerOptions::new().name(name);
        #[cfg(unix)]
        if let Some(mode) = self.mode {
            opts = opts.mode(mode);
        }
        #[cfg(windows)]
        if let Some(sddl) = &self.sddl {
            let sddl = U16CString::from_str(sddl)
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            let sd = SecurityDescriptor::deserialize(&sddl)?;
            opts = opts.security_descriptor(sd);
        }

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let listener = opts
                .create_tokio()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,

                        res = listener.accept() => {
                            let socket = match res {
                                Ok(s) => s,
                                Err(_) => break,
                            };

                            let (r, w) = socket.split();

                            *reader.lock().await = Some(r);
                            *writer.lock().await = Some(w);
                        }
                    }
                }
            });

            Ok(())
        })
    }

    pub fn stop(&self) -> PyResult<()> {
        self.shutdown.cancel();
        Ok(())
    }
}


*/
