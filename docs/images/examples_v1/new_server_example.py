import asyncio

from taunicorn import Server


async def run_server():
    server = Server("demo_socket")

    print("[SERVER] starting …")
    await server.serve()

    print("[SERVER] waiting for client …")

    while True:
        try:
            data = await server.receive()
            if data is not None:

                print("[SERVER] received:", data)

                await server.send(b"echo: " + data)
        except RuntimeError:
            await asyncio.sleep(0.05)

    # server.stop()
    print("[SERVER] stopped")


if __name__ == "__main__":
    asyncio.run(run_server())
