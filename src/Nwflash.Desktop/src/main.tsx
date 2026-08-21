import React from 'react';
import { createRoot } from 'react-dom/client';
import { initializeE2eBridge } from '@nwflash/e2e-bridge';
import { App } from './app/App';
import './styles/app.css';

void initializeE2eBridge();

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
