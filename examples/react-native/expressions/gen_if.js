function* g(x){ if (x) { yield 1; } else { yield 2; } yield 3; } print([...g(true)].join(), [...g(false)].join());
