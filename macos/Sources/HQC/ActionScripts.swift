// The page scripts of the shortcut actions, run in the active tab by callAsyncJavaScript (a
// function body; it may await). They drive the hqc client's own controls and resolve to
// whether they did something. The session column is `aside.sidebar`; a narrow window (until
// "Sessions" slides it out), the search page and the phone layout have none, and then the
// session actions do nothing.
extension ShortcutAction {

    var script: String {
        switch self {
        case .newSession: return Self.newSessionScript
        case .archiveSession: return Self.archiveScript
        case .previousSession: return Self.sessionStepScript(-1)
        case .nextSession: return Self.sessionStepScript(1)
        case .previousProject: return Self.projectStepScript(-1)
        case .nextProject: return Self.projectStepScript(1)
        }
    }

    // Clicks the client's own "New session" button as soon as it is there and enabled (it is
    // disabled until the client has loaded the user), checking every 100 ms for up to 5 s.
    // Where the button is absent the script first brings the live list back, each step at most
    // once, and waits for the list's button: it leaves the column's archive mode, slides the list
    // out in a narrow window ("Sessions" in the chat header), leaves the search page, or follows
    // the page's "Back to sessions" link (the phone layout's chat, Project settings). Resolves to
    // whether it clicked.
    private static let newSessionScript = """
        return await new Promise((resolve) => {
          const started = Date.now();
          let leftArchive = false, slidOut = false, leftSearch = false, wentBack = false;
          const check = () => {
            const b = document.querySelector('button.btn-new-session');
            if (b && !b.disabled) { b.click(); resolve(true); return; }
            if (!b) {
              const archive = document.querySelector('button.btn-archive-back');
              const toggle = document.querySelector('button.btn-sessions');
              const search = document.querySelector('a.sp-back');
              const back = document.querySelector('a[aria-label="Back to sessions"]:not(.requests-screen *)');
              if (archive && !leftArchive) { archive.click(); leftArchive = true; }
              else if (toggle && !slidOut) { toggle.click(); slidOut = true; }
              else if (!toggle && search && !leftSearch) { search.click(); leftSearch = true; }
              else if (!toggle && back && !wentBack) { back.click(); wentBack = true; }
            }
            if (Date.now() - started >= 5000) { resolve(false); }
            else { setTimeout(check, 100); }
          };
          check();
        });
        """

    // Archives the open session (`#/s/<id>`) as its row's menu does, Undo toast included: a
    // right click on the row opens the menu, then its Archive item is clicked once it renders
    // (up to 1 s) — only the menu of that row (`is-menu`), never one already open on another.
    // No row in the column (an archived session, archive mode), no menu or no Archive item:
    // nothing.
    private static let archiveScript = """
        const m = location.hash.match(/^#\\/s\\/([^/?#]+)/);
        const side = document.querySelector('aside.sidebar');
        if (!m || !side) return false;
        const href = '#/s/' + decodeURIComponent(m[1]);
        const row = [...side.querySelectorAll('a.session-row')].find((a) => a.getAttribute('href') === href);
        if (!row) return false;
        row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
        return await new Promise((resolve) => {
          const started = Date.now();
          const check = () => {
            const item = row.classList.contains('is-menu') && side.querySelector('.ctx-menu button.btn-archive');
            if (item) { item.click(); resolve(true); }
            else if (Date.now() - started >= 1000) { resolve(false); }
            else { setTimeout(check, 50); }
          };
          check();
        });
        """

    // Opens the session row above (`step` -1) or below (+1) the current one in the column's
    // order: Favorites, then the draft and the groups. With no current row, the last or the
    // first row; past either end, nothing.
    private static func sessionStepScript(_ step: Int) -> String {
        """
        const side = document.querySelector('aside.sidebar');
        if (!side) return false;
        const rows = [...side.querySelectorAll('a.session-row')];
        const at = rows.findIndex((a) => a.classList.contains('is-current'));
        const to = at < 0 ? rows[\(step < 0 ? "rows.length - 1" : "0")] : rows[at + (\(step))];
        if (!to) return false;
        to.click();
        return true;
        """
    }

    // Opens the project above (`step` -1) or below (+1) the current one on the project rail.
    // With no current project, or past either end, nothing.
    private static func projectStepScript(_ step: Int) -> String {
        """
        const rail = document.querySelector('nav.rail');
        if (!rail) return false;
        const tiles = [...rail.querySelectorAll('a.rail-project')];
        const at = tiles.findIndex((a) => a.classList.contains('is-current'));
        const to = at < 0 ? undefined : tiles[at + (\(step))];
        if (!to) return false;
        to.click();
        return true;
        """
    }
}
