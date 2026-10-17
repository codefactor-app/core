interface Repository { query(sql: string): string; }

class Store implements Repository {
  private url: string;
  constructor(url: string) { this.url = url; }
  query(sql: string): string { return sql; }
}

function overload(x: string): string;
function overload(x: number): number;
function overload(x: string | number): string | number { return x; }

function entry(items: string[], repo: Store) {
  for (const item of items) {
    if (item) repo.query(item);
    else overload(item);
  }
}
