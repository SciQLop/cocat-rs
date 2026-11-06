// A table marked `data-sortable` sorts by a column when its heading is clicked; a second click
// reverses the order. A cell's `data-sort` holds the value to compare (a byte count, a date)
// when its text is not that value; numbers compare as numbers, the rest as text.
(function () {
  function key(row, column) {
    var cell = row.cells[column];
    var value = cell ? (cell.getAttribute("data-sort") || cell.textContent.trim()) : "";
    var number = Number(value);
    return value !== "" && !isNaN(number) ? number : value.toLowerCase();
  }

  function sort(table, column, ascending) {
    var body = table.tBodies[0];
    var rows = Array.prototype.slice.call(body.rows);
    rows.sort(function (a, b) {
      var left = key(a, column);
      var right = key(b, column);
      var order = left < right ? -1 : left > right ? 1 : 0;
      return ascending ? order : -order;
    });
    rows.forEach(function (row) { body.appendChild(row); });
  }

  document.querySelectorAll("table[data-sortable]").forEach(function (table) {
    var headings = table.tHead.rows[0].cells;
    Array.prototype.forEach.call(headings, function (heading, column) {
      if (heading.classList.contains("actions")) {
        return;
      }
      var button = document.createElement("button");
      button.type = "button";
      button.className = "sort";
      button.textContent = heading.textContent;
      heading.textContent = "";
      heading.appendChild(button);
      button.addEventListener("click", function () {
        var ascending = heading.getAttribute("aria-sort") !== "ascending";
        Array.prototype.forEach.call(headings, function (other) {
          other.removeAttribute("aria-sort");
        });
        heading.setAttribute("aria-sort", ascending ? "ascending" : "descending");
        sort(table, column, ascending);
      });
    });
  });
})();
