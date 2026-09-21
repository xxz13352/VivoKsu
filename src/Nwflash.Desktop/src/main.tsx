import React from 'react';
import { createRoot } from 'react-dom/client';
import { initializeE2eBridge } from '@nwflash/e2e-bridge';
import { App } from './app/App';
import { installWebviewHardening } from './app/webview-hardening';
import './styles/app.css';

// release 构建启用 WebView 侧加固（防 IPC 重写 + 禁调试快捷键）。
// 开发态必须放行，否则无法调试。
installWebviewHardening(!import.meta.env.DEV);

void initializeE2eBridge();

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
