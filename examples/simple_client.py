import asyncio
from taunicorn import connect, Sender, Receiver




async def handle_connection(sender: Sender, receiver: Receiver):
    await sender.send(b"ping")
    try:
        while True:
            response:bytes = await receiver.recv()
            
            if not response:
                break
            print("Response from the server:", response.decode())
            await sender.send(b"Got: " + response)
    except asyncio.CancelledError:
        print("Handler cancelled")


async def main():
    pipe_name = r"\\.\pipe\example_ipc_pipe"
    await connect(pipe_name, handle_connection)


if __name__ == "__main__":
    asyncio.run(main())