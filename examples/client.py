import asyncio
from taunicorn import connect, Sender, Receiver


async def handle_connection(sender: Sender, receiver: Receiver):
    await sender.send(b"ping")
    response:bytes = await receiver.recv()
    print("Antwort vom Server:", response.decode())


async def main():
    pipe_name = r"\\.\pipe\example_ipc_pipe"
    await connect(pipe_name, handle_connection)


if __name__ == "__main__":
    asyncio.run(main())
