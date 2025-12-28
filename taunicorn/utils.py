import json
import struct
from typing import Any, Dict, List


def encode_frame(event: Dict[str, Any]) -> bytes:
    payload = json.dumps(event, separators=(",", ":")).encode("utf-8")
    return struct.pack("!I", len(payload)) + payload


def decode_frames(buffer: bytearray) -> List[Dict[str, Any]]:
    messages = []

    while len(buffer) >= 4:
        length = struct.unpack("!I", buffer[:4])[0]
        if len(buffer) < 4 + length:
            break

        payload = buffer[4:4 + length]
        del buffer[:4 + length]

        messages.append(json.loads(payload.decode("utf-8")))

    return messages

