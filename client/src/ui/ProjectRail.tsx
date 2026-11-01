import { useState } from 'preact/hooks';
import type { AppState } from '../app';
import { ProjectGlyph, projectName, tileBackground } from './Appearance';
import { useSubscription } from './hooks';
import { IconPlus, IconSettings } from './icons';
import { newProject } from './SessionList';

/** The project bar of a desktop (T49, README "Project bar"): a tile per project with its
 *  icon on its color at ~24 %; the current one more saturated (`--mark-bg`) with a pill of
 *  the project color at the left edge; an amber "needs you" badge; "+ project", settings
 *  and who is signed in. */
export function ProjectRail({ app }: { app: AppState }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [error, setError] = useState<string | null>(null);
  const current = app.currentProjectId;
  const email = app.me?.email ?? '';
  return (
    <nav class="rail" aria-label="Projects">
      <a class="rail-logo" href="#/" title="HQC">
        <img src="/hqc-mark-small.svg" alt="HQC" width="38" height="38" />
      </a>
      <div class="rail-sep" />
      {app.sortedProjects().map((p) => {
        const name = projectName(p);
        const waiting = app.projectWaiting(p.id);
        const on = p.id === current;
        return (
          <div key={p.id} class="rail-item">
            {on && <span class="rail-pill" aria-hidden="true" />}
            <a
              class={`project-row rail-project ${on ? 'is-current' : ''}`}
              href={`#/p/${p.id}`}
              aria-label={name}
              aria-current={on ? 'true' : undefined}
              title={name}
              style={on ? undefined : { background: tileBackground(p) }}
            >
              <ProjectGlyph p={p} />
            </a>
            {waiting > 0 && (
              <span class="rail-badge" aria-label={`needs you: ${waiting}`}>
                {waiting > 99 ? '99+' : waiting}
              </span>
            )}
          </div>
        );
      })}
      <button
        type="button"
        class="rail-btn rail-new"
        aria-label="New project"
        title={error ?? 'New project'}
        disabled={!app.me}
        onClick={() => void newProject(app, setError)}
      >
        <IconPlus size={15} />
      </button>
      <div class="spacer" />
      <a class="rail-btn" href="#/settings" aria-label="Settings" title="Settings">
        <IconSettings size={18} />
      </a>
      {email && (
        <div class="avatar" title={email} aria-label={email} role="img">
          {email.slice(0, 1).toUpperCase()}
        </div>
      )}
    </nav>
  );
}
