import { FC } from 'react';
import { AppPageId, PageNavGroup } from '../app/pageManifest';

type SidebarProps = {
  navGroups: ReadonlyArray<PageNavGroup>;
  currentPage: AppPageId;
  onSelectPage: (page: AppPageId) => void;
  username: string;
  currentTime: string;
  logoutDisabled: boolean;
  onLogout: () => void;
};

export const Sidebar: FC<SidebarProps> = ({
  navGroups,
  currentPage,
  onSelectPage,
  username,
  currentTime,
  logoutDisabled,
  onLogout,
}) => (
  <aside className="nw-sidebar" aria-label="主导航">
    <nav className="nw-sidebar-nav">
      <div className="nw-nav-workspace">WORKSPACE</div>
      {navGroups.map((group) => (
        <section className="nw-nav-group" key={group.title}>
          <div className="nw-nav-title">{group.title}</div>
          <div className="nw-nav-items">
            {group.pages.map((page) => (
              <button
                type="button"
                key={page.id}
                className={`nw-nav-item ${currentPage === page.id ? 'active' : ''}`}
                data-page-id={page.id}
                onClick={() => onSelectPage(page.id)}
              >
                {page.label}
              </button>
            ))}
          </div>
        </section>
      ))}
    </nav>
    <div className="nw-sidebar-account">
      <div><span>账号</span><strong>{username}</strong></div>
      <time>{currentTime}</time>
      <button
        type="button"
        className="nw-logout-button"
        onClick={onLogout}
        disabled={logoutDisabled}
        data-role="logout-button"
        aria-label="退出登录"
      >
        登出
      </button>
    </div>
  </aside>
);
