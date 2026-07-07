import { defineConfig } from 'vite';

// The Express server (server.js) runs Vite in middleware mode, so this config
// mostly exists to keep the dev server happy and pin the project root.
export default defineConfig({
  root: '.',
  appType: 'spa',
  server: {
    hmr: true
  }
});
