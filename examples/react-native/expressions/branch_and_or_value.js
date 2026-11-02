function t(a, b, c) { let r; if (a && b || c) { r = "then"; } else { r = "else"; } return r; }
print(t(1, 1, 0), t(1, 0, 0), t(0, 0, 1), t(1, 0, 1));
