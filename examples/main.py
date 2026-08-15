import asyncio
from multiprocessing import Process

import client
import server


def run_server():
    asyncio.run(server.main())


def run_client():
    asyncio.run(client.main())


def main():
    p1 = Process(target=run_server)
    p2 = Process(target=run_client)
    p1.start()
    p2.start()
    p1.join()
    p2.join()


if __name__ == "__main__":
    main()
