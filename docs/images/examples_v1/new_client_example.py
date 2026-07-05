import asyncio

from taunicorn import Client


async def run_client():
    client = Client("demo_socket")

    print("[CLIENT] connecting …")
    await client.connect()

    await client.send(b"hello from client")

    reply = await client.receive()
    print("[CLIENT] reply:", reply)


if __name__ == "__main__":
    asyncio.run(run_client())
