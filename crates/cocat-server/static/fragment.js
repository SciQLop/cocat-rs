// A token in the URL fragment never reaches the server log or the history entry: move it
// into the form's hidden field, then clear the address bar. A form marked
// `data-submit-fragment` posts itself as soon as the token lands, because its page is only
// a step in the link's journey.
(function () {
  var field = document.querySelector('input[name="token"]');
  if (!field || !field.form) {
    return;
  }
  var token = window.location.hash.replace(/^#/, "");
  if (!token) {
    return;
  }
  field.value = token;
  history.replaceState(null, "", window.location.pathname + window.location.search);
  if (field.form.hasAttribute("data-submit-fragment")) {
    field.form.requestSubmit();
  }
})();
