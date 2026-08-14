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
use tokio_util::sync::CancellationToken;

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
  shutdown: CancellationToken,
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
      shutdown: CancellationToken::new(),
      #[cfg(windows)]
      sddl,
    })
  }

  pub fn start<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    let name = self.name.clone();
    let pool = self.pool.clone();
    let incoming = self.incoming_tx.clone();
    let shutdown = self.shutdown.clone();

    let mut opts = ListenerOptions::new().name(name);

    #[cfg(windows)]
    if let Some(sddl) = &self.sddl {
      let sddl = U16CString::from_str(sddl).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
      let sd = SecurityDescriptor::deserialize(&sddl)?;
      opts = opts.security_descriptor(sd);
    }

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
      let listener = opts
        .create_tokio()
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

      // ===== ACCEPT LOOP =====
      tokio::spawn(async move {
        loop {
          tokio::select! {
              _ = shutdown.cancelled() => {
                  break;
              }

              res = listener.accept() => {
                  let socket = match res {
                      Ok(s) => s,
                      Err(_) => break,
                  };

                  let (reader, writer) = socket.split();
                  let stream = Arc::new(SharedStream::new(reader, writer));
                  let client_id = pool.add(stream.clone()).await;

                  let pool_clone = pool.clone();
                  let incoming_clone = incoming.clone();
                  let shutdown_clone = shutdown.clone();

                  // ===== CLIENT RECV LOOP =====
                  tokio::spawn(async move {
                      loop {
                          tokio::select! {
                              _ = shutdown_clone.cancelled() => {
                                  break;
                              }

                              res = async {
                                  let mut r = stream.reader.lock().await;
                                  let len = r.read_u32_le().await?;
                                  let mut buf = vec![0; len as usize];
                                  r.read_exact(&mut buf).await?;
                                  Ok::<_, std::io::Error>(buf)
                              } => {
                                  let buf = match res {
                                      Ok(b) => b,
                                      Err(_) => break,
                                  };

                                  if let Ok(val) = serde_json::from_slice::<Value>(&buf) {
                                      let _ = incoming_clone.send((client_id, val)).await;
                                  }
                              }
                          }
                      }

                      pool_clone.remove(client_id).await;
                  });
              }
          }
        }
      });

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

      let json =
        Python::attach(|py| depythonize::<Value>(&obj.into_bound_py_any(py).unwrap()).unwrap());

      let payload = serde_json::to_vec(&json).unwrap();
      let mut w = stream.writer.lock().await;

      w.write_u32_le(payload.len() as u32).await?;
      w.write_all(&payload).await?;

      Ok(())
    })
  }
  pub fn stop(&self) -> PyResult<()> {
    let shutdown = self.shutdown.clone();
    shutdown.cancel();
    Ok(())
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
/*  
python.pyi


class Acceptor:
    def __init__(
        self, name: str, sddl: Optional[str] = "D:(A;;GA;;;WD)"
    ) -> None:  # ( D:(A;;GA;;;WD) ) ==> Allow Everyone access
        """
        Create a local socket acceptor with a connection pool.

        :param name: Socket / pipe name
        :param sddl: Windows-only security descriptor (optional)
        """
        ...

    async def start(self) -> None:
        """
        Start listening for incoming clients.

        This method accepts clients indefinitely in the background.
        It must be awaited exactly once before using send/recv.
        """
        ...

    async def recv(self) -> Tuple[int, Dict[str, Any]]:
        """
        Receive the next message from any connected client.

        :return: (client_id, message)
        :raises RuntimeError: if the server is closed
        """
        ...

    async def send(self, client_id: int, data: Dict[str, Any]) -> None:
        """
        Send a JSON message to a specific client.

        :param client_id: ID of the target client
        :param data: JSON-serializable dictionary
        :raises RuntimeError: if the client does not exist
        """
        ...

    def stop(self) -> None:
        """
        Stop the acceptor.

        Cancels the accept loop and all active client receive tasks.
        """
        ...




*/
