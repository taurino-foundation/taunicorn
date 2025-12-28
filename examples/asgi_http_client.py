import asyncio
from taunicorn import connect, encode_frame, decode_frames


async def handle_connection(sender, receiver):
    # Scope
    await sender.send(encode_frame({
        "type": "asgi.scope",
        "scope": {
            "type": "http",
            "asgi": {"version": "3.0", "spec_version": "2.5"},
            "http_version": "1.1",
            "method": "GET",
            "scheme": "http",
            "path": "/ping",
            "query_string": "",
            "headers": [],
            "client": ["ipc", 0],
            "server": ["ipc", None]
        }
    }))

    # Request
    await sender.send(encode_frame({
        "type": "http.request",
        "body": "",
        "more_body": False
    }))

    buffer = bytearray()

    while True:
        data = await receiver.recv()
        if data is None:
            break

        buffer.extend(data)
        for msg in decode_frames(buffer):
            print("SERVER:", msg)


async def main():
    await connect("main_pipe_file", handle_connection)


if __name__ == "__main__":
    asyncio.run(main())
