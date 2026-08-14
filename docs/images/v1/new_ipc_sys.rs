use interprocess::local_socket::tokio::prelude::LocalSocketListener;
use interprocess::local_socket::tokio::{RecvHalf, SendHalf, Stream};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ToFsName as _, ToNsName,
};
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

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use anyhow::Result;
use std::sync::Arc;

fn get_ocket_name(
    name: &str,
) -> Result<interprocess::local_socket::Name<'static>> {
    if let Ok(ns_name) = name.to_string().to_ns_name::<GenericNamespaced>() {
        return Ok(ns_name);
    }

    let path = if cfg!(unix) {
        if name.starts_with('/') {
            name.to_string()
        } else {
            format!("/tmp/{}.sock", name)
        }
    } else if name.starts_with(r"\\.\pipe\") {
        name.to_string()
    } else {
        format!(r"\\.\pipe\{}", name)
    };

    path.to_fs_name::<GenericFilePath>()
        .map_err(|e| anyhow::anyhow!(e))
}

async fn read_frame<R: AsyncReadExt + Unpin>(
    r: &mut R,
) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

async fn write_frame<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    payload: &[u8],
) -> Result<()> {
    let len = payload.len() as u32;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

#[pyclass]
pub struct Client {
    writer: Arc<Mutex<SendHalf>>,
    reader: Arc<Mutex<RecvHalf>>,
}

#[pymethods]
impl Client {
    /// Python: await send(b"...")
    fn send<'py>(
        &self,
        py: Python<'py>,
        data: Bound<'py, PyBytes>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let writer = self.writer.clone();
        // Bytes aus Python kopieren (GIL-gebunden), dann GIL frei
        let payload: Vec<u8> = data.as_bytes().to_vec();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut w = writer.lock().await;
            write_frame(&mut *w, &payload)
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            Ok(())
        })
    }
    fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let reader = self.reader.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // 1️⃣ Tokio-only: Bytes lesen
            let bytes = {
                let mut r = reader.lock().await;
                match read_frame(&mut *r).await {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => {
                        return Err(PyRuntimeError::new_err(
                            "connection closed",
                        ));
                    }
                    Err(e) => {
                        return Err(PyRuntimeError::new_err(e.to_string()));
                    }
                }
            };

            // 2️⃣ Python-Objekt explizit erzeugen
            Python::attach(|py| {
                let py_bytes = PyBytes::new(py, &bytes);
                let obj: Py<PyAny> = py_bytes.into(); // ✅ DAS ist der Trick
                Ok(obj)
            })
        })
    }
}

#[pyfunction]
pub fn connect<'py>(
    py: Python<'py>,
    name: String,
) -> PyResult<Bound<'py, PyAny>> {
    let name = get_ocket_name(&name)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let socket = Stream::connect(name).await.map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyIOError, _>(e.to_string())
        })?;

        let (reader, writer) = socket.split();

        let client = Python::attach(|py| {
            Py::new(
                py,
                Client {
                    reader: Arc::new(Mutex::new(reader)),
                    writer: Arc::new(Mutex::new(writer)),
                },
            )
        })?;

        Ok(client)
    })
}

#[pyclass]
pub struct Connection {
    writer: Arc<Mutex<SendHalf>>,
    reader: Arc<Mutex<RecvHalf>>,
}

#[pymethods]
impl Connection {
    /// Python: await send(b"...")
    fn send<'py>(
        &self,
        py: Python<'py>,
        data: Bound<'py, PyBytes>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let writer = self.writer.clone();
        // Bytes aus Python kopieren (GIL-gebunden), dann GIL frei
        let payload: Vec<u8> = data.as_bytes().to_vec();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut w = writer.lock().await;
            write_frame(&mut *w, &payload)
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            Ok(())
        })
    }
    fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let reader = self.reader.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // 1️⃣ Tokio-only: Bytes lesen
            let bytes = {
                let mut r = reader.lock().await;
                match read_frame(&mut *r).await {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => {
                        return Err(PyRuntimeError::new_err(
                            "connection closed",
                        ));
                    }
                    Err(e) => {
                        return Err(PyRuntimeError::new_err(e.to_string()));
                    }
                }
            };

            // 2️⃣ Python-Objekt explizit erzeugen
            Python::attach(|py| {
                let py_bytes = PyBytes::new(py, &bytes);
                let obj: Py<PyAny> = py_bytes.into(); // ✅ DAS ist der Trick
                Ok(obj)
            })
        })
    }
}
#[pyclass]
pub struct Server {
    listener: Arc<Mutex<LocalSocketListener>>,
}
#[pymethods]
impl Server {
    fn accept<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let listener = self.listener.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let stream = listener
                .lock()
                .await
                .accept()
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

            let (reader, writer) = stream.split();

            let conn = Python::attach(|py| {
                Py::new(
                    py,
                    Connection {
                        reader: Arc::new(Mutex::new(reader)),
                        writer: Arc::new(Mutex::new(writer)),
                    },
                )
            })?;

            Ok(conn)
        })
    }
}

#[pyfunction]
#[pyo3(signature=(name, *, mode=None, sddl=None))]
pub fn serve<'py>(
    py: Python<'py>,
    name: String,
    mode: Option<u32>,
    sddl: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    #[cfg(windows)]
    let _ = mode;

    let name = get_ocket_name(&name)?;

    let mut opts = ListenerOptions::new().name(name);

    #[cfg(unix)]
    if let Some(m) = mode {
        opts = opts.mode(m as libc::mode_t);
    }

    #[cfg(windows)]
    if let Some(sddl) = &sddl {
        use widestring::U16CString;
        let sddl = U16CString::from_str(sddl)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let sd = SecurityDescriptor::deserialize(&sddl)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        opts = opts.security_descriptor(sd);
    }

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let listener = opts
            .create_tokio()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        let server = Python::attach(|py| {
            Py::new(
                py,
                Server {
                    listener: Arc::new(Mutex::new(listener)),
                },
            )
        })?;

        Ok(server)
    })
}
