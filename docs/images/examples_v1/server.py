import asyncio
import math
import time
import uuid
from typing import Dict, Any

from taunicorn import MPSCChannel, BroadcastChannel, Acceptor


class ComputeServer:
    def __init__(self, workers: int = 2) -> None:
        self.acceptor = Acceptor("compute-socket", sddl="D:(A;;GA;;;WD)")
        self.queue = MPSCChannel(buffer=128)
        self.broadcast = BroadcastChannel(buffer=32)
        self.workers = workers

    async def start(self) -> None:
        await self.acceptor.start()
        asyncio.create_task(self._accept_clients())

        for wid in range(self.workers):
            asyncio.create_task(self._worker(wid))

    async def _accept_clients(self) -> None:
        while True:
            client_id, msg = await self.acceptor.recv()

            job_id = str(uuid.uuid4())
            await self.queue.send({
                "job_id": job_id,
                "client_id": client_id,
                "payload": msg,
                "t_queued": time.perf_counter(),
            })

            await self.broadcast.send({
                "type": "queued",
                "job_id": job_id,
                "client_id": client_id,
            })

    async def _worker(self, worker_id: int) -> None:
        while True:
            job = await self.queue.recv()
            if job is None:
                break

            job_id = job["job_id"]
            client_id = job["client_id"]
            matrix = job["payload"]["matrix"]

            t_start = time.perf_counter()

            await self.broadcast.send({
                "type": "running",
                "job_id": job_id,
                "worker": worker_id,
            })

            result = await self._compute(job_id, matrix)

            t_done = time.perf_counter()

            await self.acceptor.send(client_id, {
                "type": "done",
                "job_id": job_id,
                "result": result,
                "timing": {
                    "queued_to_start": t_start - job["t_queued"],
                    "runtime": t_done - t_start,
                    "total": t_done - job["t_queued"],
                },
            })

            await self.broadcast.send({
                "type": "done",
                "job_id": job_id,
                "worker": worker_id,
                "duration": t_done - t_start,
            })

    async def _compute(self, job_id: str, matrix: list[list[float]]) -> Dict[str, Any]:
        size = len(matrix)
        result = [[0.0] * size for _ in range(size)]

        total = size * size
        step = max(1, total // 10)
        processed = 0

        for i in range(size):
            for j in range(size):
                for k in range(size):
                    result[i][j] += matrix[i][k] * matrix[j][k]

                processed += 1
                if processed % step == 0:
                    await self.broadcast.send({
                        "type": "progress",
                        "job_id": job_id,
                        "progress": int(processed / total * 100),
                    })

                await asyncio.sleep(0)  # cooperative scheduling

        flat = [x for row in result for x in row]
        mean = sum(flat) / len(flat)
        variance = sum((x - mean) ** 2 for x in flat) / len(flat)

        return {
            "stats": {
                "mean": mean,
                "variance": variance,
                "stddev": math.sqrt(variance),
            }
        }
