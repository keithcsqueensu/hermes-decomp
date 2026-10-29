// versions: 98 99
// S3.4: an async function's parameter is copied into its machine's scope,
// and its response is another variable there, named after the key the
// parameter is sent under. The parameter's copy may print as the parameter;
// the response must stay a variable of its own.
(function () {
  var log = [];
  function post(req) { return Promise.resolve({ body: { echoed: req.body.action_type } }); }
  async function fireSurveyAction(action_type, metadata) {
    var body = { action_type: action_type };
    if (metadata != null) body.metadata = metadata;
    var response = await post({ body: body });
    log.push(action_type + "->" + response.body.echoed);
    return response;
  }
  fireSurveyAction("open", null).then(function () { print(log.join(" ")); });
})();
