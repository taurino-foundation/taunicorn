import asyncio

from .server import ComputeServer
from .client import ComputeClient


async def main() -> None:
    server = ComputeServer(workers=2)
    await server.start()

    broadcast = server.broadcast

    clients = [
        ComputeClient("client-A", broadcast),
        ComputeClient("client-B", broadcast),
        ComputeClient("client-C", broadcast),
    ]

    await asyncio.gather(*(c.start() for c in clients))


if __name__ == "__main__":
    asyncio.run(main())
