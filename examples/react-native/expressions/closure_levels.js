function outer(seed){var alpha=seed+1;function mid(m){var beta=m*2;function inner(x){return alpha+beta+x;}function reader(){return alpha;}beta=beta+alpha;return [inner,reader];}return mid;}
var fns=outer(1)(2); print(fns[0](3)); print(fns[1]());
function genMethod(moduleID,methodID,type){var fn=null;if(type==="promise"){fn=function(){var args=[];for(var i=0;i<arguments.length;i++){args.push(arguments[i]);}var err="E";return function(resolve,reject){bridge(moduleID,methodID,args,function(d){return resolve(d);},function(e){return reject(e+err);});};};}return fn;}
function bridge(a,b,c,ok,bad){print(a,b,c.join("+"));ok("data");bad("fail");}
genMethod(7,9,"promise")(1,2)(function(d){print("resolved",d);},function(e){print("rejected",e);});
