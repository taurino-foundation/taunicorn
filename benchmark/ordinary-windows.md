# Ordinary Connection: local Windows baseline

Three fresh Node processes, one listener and one connected pair at a time. Windows 11 build 26300, x64, AMD Ryzen 7 4700U, Node 24.21.0. Both endpoints use the same process and existing native JS binding; these are application-level measurements, not Rust-only timings or release thresholds. Dirty main at `0c9b49583f72f2b23415a1ea8cbff3cebf1594e7`; the measured binary, not HEAD, identifies this baseline.

From the repository root (PowerShell):

```powershell
$env:TOKIO_WORKER_THREADS = '2'
$env:UV_THREADPOOL_SIZE = '4'
$env:NAPI_RS_NATIVE_LIBRARY_PATH = (Resolve-Path javascript/taunicorn/taunicorn.win32-x64-msvc.node).Path
Get-FileHash $env:NAPI_RS_NATIVE_LIBRARY_PATH -Algorithm SHA256
1..3 | ForEach-Object { node benchmark/ordinary-duplex.mjs; if ($LASTEXITCODE) { throw 'benchmark failed' } }
```

Measured binding SHA-256: `29d78a0793c7a62398b897c4d18d98ea64acbe3761c2c80dfb9a187417b490a1`.

| Run | 64 B echo RTT p50 (ms) | p99 (ms) | Aggregate duplex (MiB/s) | Peak process working set (KiB) | Reconnect + new bytes |
| --- | ---------------------: | -------: | -----------------------: | -----------------------------: | --------------------- |
| 1   |                  2.643 |   12.460 |                    6.943 |                          44800 | passed                |
| 2   |                  3.220 |   19.454 |                    5.719 |                          44756 | passed                |
| 3   |                  2.653 |   43.388 |                    5.717 |                          44884 | passed                |

RTT excludes setup: 20 warmups, then 100 sequential request/echo samples, nearest-rank percentiles. Throughput includes receive, payload verification, and binding overhead: 32 concurrent bidirectional exchanges of 4096 B per direction (256 KiB total). Reconnect uses the same listener after both old connections close. Peak RSS is the process lifetime high-water mark, including Node, native runtime, both endpoints and setup; it is not incremental connection memory. The workload and runtime settings were fixed, not host load or CPU affinity. Do not extrapolate these results to other hosts or backends.
