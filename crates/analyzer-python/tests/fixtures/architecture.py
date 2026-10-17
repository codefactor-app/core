from httpx import get as http_get

class Store:
    def query(self, sql: str):
        return sql

class Service:
    def __init__(self, store: Store):
        self.store = store

    async def run(self, items: list[str]):
        for item in items:
            if item:
                self.store.query(item)
            else:
                http_get(item)

def factory() -> Store:
    return Store()

def entry():
    store = Store()
    store.query("select 1")
    return factory()
