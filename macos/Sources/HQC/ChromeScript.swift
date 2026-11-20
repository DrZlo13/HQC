// The page script behind the window's bar colour, injected at document start into every tab's
// main frame. It posts the project bar's colour (`nav.rail`, `background: var(--rail)` in the
// client's styles.css) to the `hqcChrome` handler as `{background: "rgb(…)"}`, else, where the
// page has no project bar (the phone layout, a page before the bar renders), the page's own
// (`html, body { background: var(--bg) }`). It posts again when the client changes the colours
// (applyTheme() in client/src/theme.ts sets the theme and the project's colour as variables on
// the root element's `style`) and when the project bar comes or goes. The bar's breadcrumbs,
// `{crumbs}`, come from NotificationScript, which keeps the sessions and the project names.
enum ChromeScript {

    static let handlerName = "hqcChrome"

    static let source = """
        (() => {
          const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.hqcChrome;
          if (!handler) return;
          let rail = null;
          let last = null;
          const post = () => {
            const el = rail || document.body;
            if (!el) return;
            const background = getComputedStyle(el).backgroundColor;
            if (background === last) return;
            last = background;
            handler.postMessage({ background });
          };
          const find = () => {
            const found = document.querySelector('nav.rail');
            if (found === rail) return;
            rail = found;
            post();
          };
          document.addEventListener('DOMContentLoaded', () => { find(); post(); });
          new MutationObserver(post).observe(document.documentElement, { attributes: true, attributeFilter: ['style'] });
          new MutationObserver(find).observe(document.documentElement, { childList: true, subtree: true });
        })();
        """
}
