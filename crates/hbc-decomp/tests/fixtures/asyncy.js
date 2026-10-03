async function fetchTwice(a) {
  const first = await a();
  const second = await a();
  return first + second;
}

print(typeof fetchTwice);
