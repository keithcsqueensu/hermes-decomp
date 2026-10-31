async function f(p){ try { return await p; } catch(e){ return await Promise.resolve("caught " + e); } } f(Promise.reject("x")).then(r=>print(r));
