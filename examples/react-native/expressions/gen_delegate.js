function* inner(){ var x = yield 1; yield x*2; return 7; } function* outer(){ var r = yield* inner(); yield r; } var it=outer(); print(it.next().value, it.next(5).value, it.next().value);
