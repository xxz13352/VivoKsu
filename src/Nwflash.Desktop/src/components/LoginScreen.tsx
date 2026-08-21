import { FC, useState } from 'react';
import loginBrandLogo from '../../../VivoKsu.App/Assets/logo.jpg';

type LoginScreenProps = {
  username: string;
  password: string;
  notice: string;
  busy: boolean;
  canSubmit: boolean;
  onUsernameChange: (value: string) => void;
  onPasswordChange: (value: string) => void;
  onSubmit: () => void;
  onClose: () => void;
};

export const LoginScreen: FC<LoginScreenProps> = ({
  username,
  password,
  notice,
  busy,
  canSubmit,
  onUsernameChange,
  onPasswordChange,
  onSubmit,
  onClose,
}) => {
  const [isPasswordVisible, setIsPasswordVisible] = useState(false);

  return (
    <main className="nw-login-screen" data-role="login-window">
      <section className="nw-login-card" aria-label="登录 奶蛙Flash">
        <header className="nw-login-brand">
          <img className="nw-login-mark" src={loginBrandLogo} alt="奶蛙Flash" />
          <div>
            <strong>奶蛙Flash</strong>
            <p>ROM CONTROL · API.NWFLASH.CC.CD</p>
          </div>
          <button
            type="button"
            className="nw-login-close"
            onClick={onClose}
            aria-label="关闭"
            disabled={busy}
          >
            ✕
          </button>
        </header>

        <div className="nw-login-heading">
          <p>SECURE SIGN IN</p>
          <h1>登录 奶蛙Flash</h1>
          <span>商业工具授权。使用后台分配的账号登录后进入主界面。</span>
        </div>

        <label className="nw-login-field">
          <span>账号</span>
          <input
            data-role="login-username"
            value={username}
            onChange={(event) => onUsernameChange(event.currentTarget.value)}
            aria-label="账号"
            autoComplete="username"
            disabled={busy}
          />
        </label>

        <label className="nw-login-field">
          <span>密码</span>
          <div className="nw-login-password">
            <input
              data-role="login-password"
              type={isPasswordVisible ? 'text' : 'password'}
              value={password}
              onChange={(event) => onPasswordChange(event.currentTarget.value)}
              aria-label="密码"
              autoComplete="current-password"
              disabled={busy}
            />
            <button
              type="button"
              className="nw-login-password-toggle"
              onClick={() => setIsPasswordVisible((value) => !value)}
              aria-label="显示或隐藏密码"
              aria-pressed={isPasswordVisible}
              disabled={busy}
            >
              {isPasswordVisible ? '\uE8D2' : '\uE8D3'}
            </button>
          </div>
        </label>

        <p className="nw-login-notice" role="status">{notice}</p>
        <button
          type="button"
          className="nw-login-submit"
          onClick={onSubmit}
          aria-label="点击登录"
          disabled={!canSubmit}
        >
          {busy ? '正在登录…' : '登 录'}
        </button>
      </section>
    </main>
  );
};
