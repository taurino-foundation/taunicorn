use interprocess::local_socket::Name;

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::{GenericFilePath, ToFsName as _, traits::tokio::Stream as _};

use pyo3::exceptions::PyRuntimeError;
use pyo3::types::PyDict;
use pyo3::{IntoPyObjectExt, prelude::*};

use pythonize::{depythonize, pythonize};
use serde_json::Value;

use interprocess::local_socket::tokio::{RecvHalf, SendHalf};
use std::{path::PathBuf, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

pub struct SharedStream {
    reader: Mutex<Option<RecvHalf>>,
    writer: Mutex<Option<SendHalf>>,
}

impl SharedStream {
    pub fn new() -> Self {
        Self {
            reader: Mutex::new(None),
            writer: Mutex::new(None),
        }
    }
}

#[pyclass]
pub struct Connector {
    name: Name<'static>,
    stream: Arc<SharedStream>,
}

#[pymethods]
impl Connector {
    #[new]
    pub fn new(name: String) -> PyResult<Self> {
        #[cfg(windows)]
        let path = format!(r"\\.\pipe\{}", name);
        #[cfg(unix)]
        let path = format!("/tmp/{}", name);

        let name = PathBuf::from(path)
            .to_fs_name::<GenericFilePath>()?
            .into_owned();

        Ok(Self {
            name,
            stream: Arc::new(SharedStream::new()),
        })
    }
    pub fn start<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let name = self.name.clone();
        let stream = self.stream.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let socket = Stream::connect(name)
                .await
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(e.to_string()))?;

            let (reader, writer) = socket.split();

            *stream.reader.lock().await = Some(reader);
            *stream.writer.lock().await = Some(writer);

            Ok(())
        })
    }
    pub fn send<'py>(
        &self,
        py: Python<'py>,
        data: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.stream.clone();
        let obj = data.into_py_any(py)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let json = Python::attach(|py| {
                depythonize::<Value>(&obj.into_bound_py_any(py).unwrap()).unwrap()
            });

            let payload = serde_json::to_vec(&json).unwrap();
            let len = payload.len() as u32;

            let mut writer = stream.writer.lock().await;
            let w = writer
                .as_mut()
                .ok_or_else(|| PyRuntimeError::new_err("not connected"))?;

            w.write_u32_le(len).await?;
            w.write_all(&payload).await?;

            Ok(())
        })
    }
    pub fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.stream.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut reader = stream.reader.lock().await;
            let r = reader
                .as_mut()
                .ok_or_else(|| PyRuntimeError::new_err("not connected"))?;

            let len = r.read_u32_le().await?;
            let mut buf = vec![0u8; len as usize];
            r.read_exact(&mut buf).await?;

            let json: Value =
                serde_json::from_slice(&buf).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

            Python::attach(|py| {
                let obj = pythonize(py, &json).unwrap();
                Ok(obj.into_py_any(py).unwrap())
            })
        })
    }
}
