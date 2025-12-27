import asyncio
from taunicorn import serve, Sender, Receiver


async def handle_client(sender: Sender, receiver: Receiver):
    print("Client verbunden")

    while True:
        data:bytes = await receiver.recv()
        if data is None:
            print("Client getrennt")
            break

        print("Empfangen:", data.decode())

        # Antwort senden
        await sender.send(b"pong")


async def main():
    pipe_name = r"\\.\pipe\example_ipc_pipe"

    await serve(
        path=pipe_name,
        handler=handle_client,
        sddl=None
    )


if __name__ == "__main__":
    asyncio.run(main())
