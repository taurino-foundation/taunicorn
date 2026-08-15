import asyncio
import json
import shlex

from taunicorn import Connection

SOCKET_NAME = "taunicorn-example"
MAX_MESSAGE_SIZE = 1024 * 1024


async def read_exact(
    connection: Connection,
    size: int,
) -> bytes:
    data = bytearray()

    while len(data) < size:
        chunk = await connection.receive(size - len(data))

        if not chunk:
            raise EOFError("server closed its sending direction")

        data.extend(chunk)

    return bytes(data)


async def receive_json(connection: Connection) -> dict:
    header = await read_exact(connection, 4)

    size = int.from_bytes(header, "big")

    if size <= 0 or size > MAX_MESSAGE_SIZE:
        raise ValueError(f"invalid message size: {size}")

    payload = await read_exact(connection, size)

    message = json.loads(payload.decode("utf-8"))

    if not isinstance(message, dict):
        raise ValueError("response is not a JSON object")

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
        raise ValueError("request too large")

    await connection.send(len(payload).to_bytes(4, "big") + payload)


def parse_number(value: str) -> int | float:
    number = float(value)

    if number.is_integer():
        return int(number)

    return number


def parse_command(line: str) -> dict | None:
    parts = shlex.split(line)

    if not parts:
        return None

    command = parts[0].lower()

    if command == "ping":
        return {
            "cmd": "PING",
        }

    if command == "echo":
        return {
            "cmd": "ECHO",
            "message": " ".join(parts[1:]),
        }

    if command == "add":
        if len(parts) < 2:
            raise ValueError("usage: add <number> [number ...]")

        return {
            "cmd": "ADD",
            "values": [parse_number(value) for value in parts[1:]],
        }

    if command == "sleep":
        seconds = 1.0

        if len(parts) >= 2:
            seconds = float(parts[1])

        return {
            "cmd": "SLEEP",
            "seconds": seconds,
        }

    if command == "stats":
        return {
            "cmd": "STATS",
        }

    if command in {"quit", "exit"}:
        return {
            "cmd": "QUIT",
        }

    if command == "help":
        print_help()
        return None

    raise ValueError(f"unknown command: {command!r}; type 'help'")


def print_help() -> None:
    print("""
Commands:

  ping
      Test server connection.

  echo <text>
      Server sends the text back.

  add <n1> <n2> [...]
      Add numbers on the server.

  sleep <seconds>
      Async server-side delay, maximum 5 seconds.

  stats
      Show server statistics.

  quit
      Close the connection.

Examples:

  ping
  echo Hallo Welt
  add 10 20 3.5
  sleep 2
  stats
""".strip())


async def main() -> None:
    print(f"Verbinde mit {SOCKET_NAME!r}...")

    connection = await Connection.connect(SOCKET_NAME)

    print("Verbunden.")
    print("Gib 'help' für die verfügbaren Befehle ein.")

    try:
        while True:
            try:
                # input() nicht direkt im Event-Loop blockieren.
                line = await asyncio.to_thread(
                    input,
                    "taunicorn> ",
                )
            except EOFError:
                print()
                break

            try:
                request = parse_command(line)
            except ValueError as exc:
                print(f"Fehler: {exc}")
                continue

            if request is None:
                continue

            await send_json(connection, request)

            try:
                response = await receive_json(connection)
            except EOFError:
                print("Server hat die Verbindung beendet.")
                break

            print(
                json.dumps(
                    response,
                    indent=2,
                    ensure_ascii=False,
                )
            )

            if request["cmd"] == "QUIT":
                # Der Server hat nach seiner QUIT-Antwort
                # shutdown_write() ausgeführt.
                remaining = await connection.receive(1)

                assert remaining == b""
                assert connection.at_eof()

                break

    finally:
        try:
            await connection.shutdown_write()
        finally:
            await connection.close()
