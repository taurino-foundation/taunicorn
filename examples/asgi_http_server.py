import asyncio
from taunicorn import server, Sender, Receiver, encode_frame, decode_frames




class IPCASGIConnection:
    def __init__(self, sender:Sender, receiver:Receiver):
        self.sender:Sender = sender
        self.receiver:Receiver = receiver
        self._buffer = bytearray()

    async def receive(self):
        while True:
            data = await self.receiver.recv()
            if data is None:
                return {"type": "http.disconnect"}

            self._buffer.extend(data)
            msgs = decode_frames(self._buffer)
            if msgs:
                return msgs.pop(0)

    async def send(self, message):
        await self.sender.send(encode_frame(message))

async def app(scope, receive:Receiver, send:Sender):
    if scope["type"] == "http":
        event = await receive()

        if event["type"] == "http.request":
            await send({
                "type": "http.response.start",
                "status": 200,
                "headers": [
                    ["content-type", "text/plain"]
                ]
            })

            await send({
                "type": "http.response.body",
                "body": "pong",
                "more_body": False
            })



async def handle_client(sender:Sender, receiver:Receiver):
    conn = IPCASGIConnection(sender, receiver)

    buffer = bytearray()

    while True:
        data = await receiver.recv()
        if data is None:
            return

        buffer.extend(data)
        msgs = decode_frames(buffer)
        if msgs:
            scope = msgs[0]["scope"]
            break

    await app(scope, conn.receive, conn.send)


async def main():
    await server(
        name="main_pipe_file",
        handler=handle_client,
        sddl=None
    )


if __name__ == "__main__":
    asyncio.run(main())