import asyncio
from taunicorn import server, Sender, Receiver


async def handle_client(sender: Sender, receiver: Receiver):
    while True:
        data:bytes = await receiver.recv()
        if data is None:
            print("Client getrennt")
            break

        print("Response from the client:", data.decode())

        await sender.send(b"pong")


async def main():
    pipe_name = "main_pipe_file"

    await server(
        name=pipe_name,
        handler=handle_client,
        sddl=None
    )


if __name__ == "__main__":
    asyncio.run(main())