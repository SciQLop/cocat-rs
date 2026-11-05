// A form marked `data-confirm` asks its question before it acts, so a destructive action the
// page offers cannot run on a stray click.
(function () {
  document.addEventListener("submit", function (event) {
    if (!event.target.hasAttribute("data-confirm")) {
      return;
    }
    if (!window.confirm(event.target.getAttribute("data-confirm"))) {
      event.preventDefault();
    }
  });
})();
