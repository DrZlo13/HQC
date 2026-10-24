import { useState } from 'preact/hooks';
import type { AppState } from '../app';
import { useSubscription } from './hooks';
import { IconPlus, IconSettings } from './icons';
import { newProject, projectName } from './SessionList';
import { projectInitials } from './sessions';

/** The project rail of a desktop (T26): letter icons with a «ждёт вас» badge, «+ проект»,
 *  settings and who is signed in. */
export function ProjectRail({ app }: { app: AppState }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [error, setError] = useState<string | null>(null);
  const current = app.currentProjectId;
  const email = app.me?.email ?? '';
  return (
    <nav class="rail" aria-label="Проекты">
      <a class="rail-logo" href="#/" title="hqc">
        hqc
      </a>
      <div class="rail-sep" />
      {app.sortedProjects().map((p) => {
        const name = projectName(p);
        const waiting = app.projectWaiting(p.id);
        return (
          <div key={p.id} class="rail-item">
            <a
              class={`project-row rail-project ${p.id === current ? 'is-current' : ''}`}
              href={`#/p/${p.id}`}
              aria-label={name}
              aria-current={p.id === current ? 'true' : undefined}
              title={name}
            >
              {projectInitials(name, p.is_personal)}
            </a>
            {waiting > 0 && (
              <span class="rail-badge" aria-label={`ждёт вас: ${waiting}`}>
                {waiting > 99 ? '99+' : waiting}
              </span>
            )}
          </div>
        );
      })}
      <button
        type="button"
        class="rail-btn rail-new"
        aria-label="Новый проект"
        title={error ?? 'Новый проект'}
        disabled={!app.me}
        onClick={() => void newProject(app, setError)}
      >
        <IconPlus size={18} />
      </button>
      <div class="spacer" />
      <a class="rail-btn" href="#/settings" aria-label="Настройки" title="Настройки">
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
