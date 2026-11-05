// A token in the URL fragment never reaches the server log or the history entry: move it
// into the form's hidden field, then clear the address bar.
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
})();
