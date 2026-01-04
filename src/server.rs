use interprocess::local_socket::Name;
use interprocess::local_socket::tokio::{RecvHalf, SendHalf};
use interprocess::local_socket::{GenericFilePath, ToFsName as _};
use interprocess::local_socket::{
    ListenerOptions,
    traits::tokio::{Listener as _, Stream as _},
};
use pyo3::exceptions::PyRuntimeError;
use pyo3::types::PyDict;
use pyo3::{IntoPyObjectExt, prelude::*};
use pythonize::{depythonize, pythonize};
use serde_json::Value;
use std::collections::HashMap;
use std::{path::PathBuf, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

#[cfg(windows)]
use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
#[cfg(windows)]
use interprocess::os::windows::security_descriptor::SecurityDescriptor;

#[cfg(windows)]
use widestring::U16CString;

pub struct SharedStream {
    reader: Mutex<RecvHalf>,
    writer: Mutex<SendHalf>,
}

impl SharedStream {
    pub fn new(reader: RecvHalf, writer: SendHalf) -> Self {
        Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
        }
    }
}

type ClientId = u64;

pub struct ConnectionPool {
    next_id: Mutex<ClientId>,
    clients: Mutex<HashMap<ClientId, Arc<SharedStream>>>,
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            next_id: Mutex::new(1),
            clients: Mutex::new(HashMap::new()),
        }
    }

    pub async fn add(&self, stream: Arc<SharedStream>) -> ClientId {
        let mut id = self.next_id.lock().await;
        let client_id = *id;
        *id += 1;

        self.clients.lock().await.insert(client_id, stream);
        client_id
    }

    pub async fn get(&self, id: ClientId) -> Option<Arc<SharedStream>> {
        self.clients.lock().await.get(&id).cloned()
    }

    pub async fn remove(&self, id: ClientId) {
        self.clients.lock().await.remove(&id);
    }
}

#[pyclass]
pub struct Acceptor {
    name: Name<'static>,
    pool: Arc<ConnectionPool>,
    incoming_tx: tokio::sync::mpsc::Sender<(ClientId, Value)>,
    incoming_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<(ClientId, Value)>>>,
    #[cfg(windows)]
    sddl: Option<String>,
}

#[pymethods]
impl Acceptor {
    #[new]
    pub fn new(name: String, #[cfg(windows)] sddl: Option<String>) -> PyResult<Self> {
        #[cfg(windows)]
        let path = format!(r"\\.\pipe\{}", name);
        #[cfg(unix)]
        let path = format!("/tmp/{}", name);

        let name = PathBuf::from(path)
            .to_fs_name::<GenericFilePath>()?
            .into_owned();
        let (tx, rx) = tokio::sync::mpsc::channel(256);

        Ok(Self {
            name,
            pool: Arc::new(ConnectionPool::new()),
            incoming_tx: tx,
            incoming_rx: Arc::new(Mutex::new(rx)),
            #[cfg(windows)]
            sddl,
        })
    }

    pub fn start<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let name = self.name.clone();
        let pool = self.pool.clone();
        let incoming = self.incoming_tx.clone();

        let mut opts = ListenerOptions::new().name(name);

        #[cfg(windows)]
        if let Some(sddl) = &self.sddl {
            let sddl =
                U16CString::from_str(sddl).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            let sd = SecurityDescriptor::deserialize(&sddl)?;
            opts = opts.security_descriptor(sd);
        }

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let listener = opts
                .create_tokio()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            tokio::spawn(async move {
                loop {
                    let socket = listener
                        .accept()
                        .await
                        .map_err(|e| PyRuntimeError::new_err(e.to_string()))
                        .unwrap();

                    let (reader, writer) = socket.split();
                    let stream = Arc::new(SharedStream::new(reader, writer));
                    let client_id = pool.add(stream.clone()).await;

                    // recv-task pro Client
                    let pool_clone = pool.clone();
                    let incoming_clone = incoming.clone();

                    tokio::spawn(async move {
                        loop {
                            let mut r = stream.reader.lock().await;
                            let len = match r.read_u32_le().await {
                                Ok(v) => v,
                                Err(_) => break,
                            };

                            let mut buf = vec![0; len as usize];
                            if r.read_exact(&mut buf).await.is_err() {
                                break;
                            }

                            if let Ok(val) = serde_json::from_slice::<Value>(&buf) {
                                let _ = incoming_clone.send((client_id, val)).await;
                            }
                        }

                        pool_clone.remove(client_id).await;
                    });
                }
            });
            // Loop is infinite, this point is never reached
            // Required for type system but marked as unreachable
            #[allow(unreachable_code)]
            Ok(Python::attach(|py| py.None()))
        })
    }

    pub fn send<'py>(
        &self,
        py: Python<'py>,
        client_id: u64,
        data: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pool = self.pool.clone();
        let obj = data.into_py_any(py)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let stream = pool
                .get(client_id)
                .await
                .ok_or_else(|| PyRuntimeError::new_err("unknown client"))?;

            let json = Python::attach(|py| {
                depythonize::<Value>(&obj.into_bound_py_any(py).unwrap()).unwrap()
            });

            let payload = serde_json::to_vec(&json).unwrap();
            let mut w = stream.writer.lock().await;

            w.write_u32_le(payload.len() as u32).await?;
            w.write_all(&payload).await?;

            Ok(())
        })
    }

    pub fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.incoming_rx.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut rx = rx.lock().await;
            let (id, value) = rx
                .recv()
                .await
                .ok_or_else(|| PyRuntimeError::new_err("server closed"))?;
            Python::attach(|py| {
                let dict = pythonize(py, &value).unwrap();
                Ok((id, dict).into_py_any(py).unwrap())
            })
        })
    }
}
