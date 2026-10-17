import { request } from 'http';

class Store {
  query(sql) { return sql; }
}

function factory() { return new Store(); }

const entry = (items) => {
  const store = new Store();
  for (const item of items) {
    if (item) store.query(item);
    else request(item);
  }
  return factory();
};
