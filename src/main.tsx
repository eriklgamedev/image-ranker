import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './styles.css';
if (import.meta.env.MODE === 'e2e') {
  await import('@wdio/tauri-plugin');
}
ReactDOM.createRoot(document.getElementById('root')!).render(<React.StrictMode><App /></React.StrictMode>);
