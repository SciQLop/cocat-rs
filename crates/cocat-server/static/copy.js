// A button marked `data-copy` copies the field it names; a share link is too long to select
// by hand.
(function () {
  document.addEventListener("click", function (event) {
    var button = event.target.closest("[data-copy]");
    if (!button) {
      return;
    }
    var field = document.getElementById(button.getAttribute("data-copy"));
    if (field) {
      navigator.clipboard.writeText(field.value);
    }
  });
})();
