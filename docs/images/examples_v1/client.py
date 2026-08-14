import asyncio
import random
from typing import Dict, Any

from taunicorn import Connector, BroadcastChannel


class ComputeClient:
    def __init__(self, name: str, broadcast: BroadcastChannel):
        self.name = name
        self.connector = Connector("compute-socket")
        self.receiver = broadcast.subscribe()

    async def start(self) -> None:
        await self.connector.start()

        asyncio.create_task(self._listen_broadcast())

        matrix = self._matrix(30)
        await self.connector.send({"matrix": matrix})

        response = await self.connector.recv()
        timing = response["timing"]

        print(
            f"[{self.name}] DONE\n"
            f"  runtime: {timing['runtime']:.3f}s\n"
            f"  queued→start: {timing['queued_to_start']:.3f}s\n"
            f"  total: {timing['total']:.3f}s\n"
        )

    async def _listen_broadcast(self) -> None:
        while True:
            msg = await self.receiver.recv()
            if msg is None:
                break

            if msg["type"] == "progress":
                print(
                    f"[{self.name}] job {msg['job_id']} "
                    f"{msg['progress']}%"
                )

            if msg["type"] == "done":
                print(
                    f"[{self.name}] job {msg['job_id']} finished "
                    f"in {msg['duration']:.3f}s"
                )

    def _matrix(self, n: int) -> list[list[float]]:
        return [[random.random() for _ in range(n)] for _ in range(n)]
