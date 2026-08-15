import asyncio
import json
import time
from dataclasses import dataclass

from taunicorn import Connection, Server

SOCKET_NAME = "taunicorn-example"
MAX_MESSAGE_SIZE = 1024 * 1024  # 1 MiB


@dataclass
class ServerState:
    started_at: float
    total_requests: int = 0
    active_clients: int = 0


async def read_exact(connection: Connection, size: int) -> bytes:
    data = bytearray()

    while len(data) < size:
        chunk = await connection.receive(size - len(data))

        if not chunk:
            raise EOFError("peer closed its sending direction")

        data.extend(chunk)

    return bytes(data)


async def receive_json(connection: Connection) -> dict:
    # Frame:
    # [4 Byte Payload-Länge][JSON Payload]
    header = await read_exact(connection, 4)
    size = int.from_bytes(header, "big")

    if size <= 0:
        raise ValueError("invalid message size")

    if size > MAX_MESSAGE_SIZE:
        raise ValueError(
            f"message too large: {size} bytes " f"(maximum: {MAX_MESSAGE_SIZE})"
        )

    payload = await read_exact(connection, size)

    try:
        message = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError("invalid JSON message") from exc

    if not isinstance(message, dict):
        raise ValueError("message must be a JSON object")

    return message


async def send_json(
    connection: Connection,
    message: dict,
) -> None:
    payload = json.dumps(
        message,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")

    if len(payload) > MAX_MESSAGE_SIZE:
        raise ValueError("response too large")

    header = len(payload).to_bytes(4, "big")

    await connection.send(header + payload)


async def execute_command(
    request: dict,
    state: ServerState,
) -> tuple[dict, bool]:
    command = request.get("cmd")

    if not isinstance(command, str):
        return {
            "ok": False,
            "error": "'cmd' must be a string",
        }, False

    command = command.upper()

    if command == "PING":
        return {
            "ok": True,
            "result": "PONG",
        }, False

    if command == "ECHO":
        return {
            "ok": True,
            "result": request.get("message"),
        }, False

    if command == "ADD":
        values = request.get("values")

        if not isinstance(values, list):
            return {
                "ok": False,
                "error": "'values' must be a list",
            }, False

        if not all(
            isinstance(value, (int, float)) and not isinstance(value, bool)
            for value in values
        ):
            return {
                "ok": False,
                "error": "'values' may only contain numbers",
            }, False

        return {
            "ok": True,
            "result": sum(values),
        }, False

    if command == "SLEEP":
        seconds = request.get("seconds", 1)

        if not isinstance(seconds, (int, float)) or isinstance(seconds, bool):
            return {
                "ok": False,
                "error": "'seconds' must be a number",
            }, False

        # Begrenzen, damit ein Client den Handler nicht
        # minutenlang beschäftigen kann.
        if not 0 <= seconds <= 5:
            return {
                "ok": False,
                "error": "'seconds' must be between 0 and 5",
            }, False

        await asyncio.sleep(seconds)

        return {
            "ok": True,
            "result": {
                "slept": seconds,
            },
        }, False

    if command == "STATS":
        return {
            "ok": True,
            "result": {
                "uptime_seconds": round(
                    time.monotonic() - state.started_at,
                    3,
                ),
                "total_requests": state.total_requests,
                "active_clients": state.active_clients,
            },
        }, False

    if command == "QUIT":
        return {
            "ok": True,
            "result": "bye",
        }, True

    return {
        "ok": False,
        "error": f"unknown command: {command}",
    }, False


async def handle_client(
    connection: Connection,
    state: ServerState,
) -> None:
    state.active_clients += 1

    print(f"[+] client connected " f"(active={state.active_clients})")

    try:
        while True:
            try:
                request = await receive_json(connection)
            except EOFError:
                # Client hat seine Schreibseite geschlossen.
                return
            except ValueError as exc:
                await send_json(
                    connection,
                    {
                        "ok": False,
                        "error": str(exc),
                    },
                )

                # Bei kaputtem Framing kann nicht zuverlässig
                # mit der Verbindung weitergearbeitet werden.
                await connection.shutdown_write()
                return

            state.total_requests += 1

            print(f"[>] {request}")

            try:
                response, close_after_response = await execute_command(request, state)
            except Exception as exc:
                # Anwendungsausnahme nicht den kompletten
                # Serverprozess beenden lassen.
                response = {
                    "ok": False,
                    "error": f"internal error: {type(exc).__name__}",
                }
                close_after_response = False

            print(f"[<] {response}")

            await send_json(connection, response)

            if close_after_response:
                # Server sendet nichts mehr.
                await connection.shutdown_write()
                return

    finally:
        state.active_clients -= 1

        await connection.close()

        print(f"[-] client disconnected " f"(active={state.active_clients})")


async def serve_forever(
    server: Server,
    state: ServerState,
) -> None:
    client_tasks: set[asyncio.Task] = set()

    try:
        while True:
            connection = await server.accept()

            task = asyncio.create_task(handle_client(connection, state))

            # Referenz behalten, solange die Task läuft.
            client_tasks.add(task)

            task.add_done_callback(client_tasks.discard)

    finally:
        # Falls der Server beendet wird, noch laufende
        # Client-Handler sauber abbrechen.
        for task in client_tasks:
            task.cancel()

        if client_tasks:
            await asyncio.gather(
                *client_tasks,
                return_exceptions=True,
            )


async def main() -> None:
    server = await Server.start(SOCKET_NAME)

    state = ServerState(
        started_at=time.monotonic(),
    )

    print(f"Server läuft auf {SOCKET_NAME!r}")
    print("Ctrl+C zum Beenden")

    try:
        await serve_forever(server, state)
    finally:
        print("Server wird beendet...")
        await server.stop()
