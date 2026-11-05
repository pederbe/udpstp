// Adds keyboard and focus behaviour to mdBook's native navigation controls.
(function () {
  var main = document.querySelector("main");
  if (!main) return;

  // The menu title holds the home and Docs links. Keep mdBook's
  // scroll-to-top handler from scrolling the page as they navigate.
  document.querySelector(".menu-title").addEventListener("click", function (event) {
    event.stopImmediatePropagation();
  }, true);

  // mdBook blurs the search field on Escape. Return focus to its opener
  // after mdBook has cleared the query and hidden the results.
  var searchToggle = document.getElementById("mdbook-search-toggle");
  var searchWrapper = document.getElementById("mdbook-search-wrapper");
  document.addEventListener("keydown", function (event) {
    if (event.key !== "Escape" || searchToggle.getAttribute("aria-expanded") !== "true") return;
    var inSearch = searchWrapper.contains(event.target) ||
      event.target === document.body && document.querySelector("#mdbook-searchresults .focus");
    if (inSearch) queueMicrotask(function () { searchToggle.focus(); });
  }, true);

  // The native button replaces mdBook's checkbox label. Keep the checkbox
  // because mdBook's layout, resizing and persistence already use it.
  var sidebar = document.getElementById("mdbook-sidebar");
  var toggle = document.getElementById("mdbook-sidebar-toggle");
  var checkbox = document.getElementById("mdbook-sidebar-toggle-anchor");
  var nextSection = 0;

  function syncNavigation() {
    // mdBook's touch and resize handlers update aria-hidden directly. Follow
    // that shared state and keep the checkbox ready for the next button click.
    var open = sidebar.getAttribute("aria-hidden") === "false";
    checkbox.checked = open;
    if (!open && sidebar.contains(document.activeElement)) toggle.focus();
    sidebar.inert = !open;
    // A reduced-motion reader never gets transitionend; close immediately
    // so browser find and keyboard navigation also exclude the sidebar.
    sidebar.style.display = open ? "" : "none";
    sidebar.querySelectorAll(".chapter-fold-toggle").forEach(function (button) {
      var item = button.closest("li");
      var sections = Array.from(item.children).find(function (child) {
        return child.tagName === "OL";
      });
      if (!sections) return;
      if (!sections.id) sections.id = "udpstp-sections-" + nextSection++;
      var link = button.previousElementSibling;
      button.setAttribute("aria-label", "Sections of " + link.textContent.trim());
      button.setAttribute("aria-controls", sections.id);
      var expanded = item.classList.contains("expanded");
      button.setAttribute("aria-expanded", expanded ? "true" : "false");
      if (!expanded && sections.contains(document.activeElement)) button.focus();
      sections.hidden = !expanded;
    });
  }

  toggle.addEventListener("click", function () {
    checkbox.checked = !checkbox.checked;
    checkbox.dispatchEvent(new Event("change"));
    syncNavigation();
  });
  sidebar.addEventListener("keydown", function (event) {
    if (event.key !== "Escape") return;
    event.preventDefault();
    checkbox.checked = false;
    checkbox.dispatchEvent(new Event("change"));
    syncNavigation();
    toggle.focus();
  });
  // mdBook adds on-this-page folds after loading and expands ancestors as
  // headings enter view. Observe that state rather than keep a second one.
  new MutationObserver(syncNavigation).observe(sidebar, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["class", "aria-hidden"]
  });
  syncNavigation();
})();
