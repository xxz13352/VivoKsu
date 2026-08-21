import { FC, useEffect } from 'react';
import { AppPageId, PageNavGroup } from '../app/pageManifest';

export type AppFrameProps = {
  appTitle: string;
  navGroups: ReadonlyArray<PageNavGroup>;
  currentPage: AppPageId;
  onSelectPage: (page: AppPageId) => void;
  progressLine: string;
  username: string;
  currentTime: string;
  onTickSecond: (value: string) => void;
  logoutDisabled: boolean;
  onLogout: () => void;
};

export const AppFrame: FC<AppFrameProps> = ({
  appTitle,
  navGroups,
  currentPage,
  onSelectPage,
  progressLine,
  username,
  currentTime,
  onTickSecond,
  logoutDisabled,
  onLogout,
}) => {
  useEffect(() => {
    const timer = setInterval(() => {
      onTickSecond(new Date().toLocaleTimeString());
    }, 1000);

    return () => clearInterval(timer);
  }, [onTickSecond]);

  return (
    <div className="nw-frame">
      <aside className="nw-sidebar">
        <div className="nw-sidebar-title">{appTitle}</div>
        {navGroups.map((group) => (
          <section className="nw-nav-group" key={group.title}>
            <div className="nw-nav-title">{group.title}</div>
            {group.pages.map((page) => (
              <button
                className={currentPage === page.id ? 'nw-nav-item active' : 'nw-nav-item'}
                type="button"
                key={page.id}
                onClick={() => onSelectPage(page.id)}
              >
                {page.label}
              </button>
            ))}
          </section>
        ))}
        <div className="nw-account">
          <span>{username}</span>
          <span>{currentTime}</span>
          <button
            type="button"
            className="nw-logout"
            disabled={logoutDisabled}
            onClick={onLogout}
          >
            退出
          </button>
        </div>
      </aside>
      <section className="nw-ops-panel">
        <div>统一进度区：{progressLine}</div>
      </section>
    </div>
  );
};
