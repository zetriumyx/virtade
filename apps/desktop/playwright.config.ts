import { defineConfig } from '@playwright/test';
import { existsSync } from 'node:fs';
export default defineConfig({testDir:'./tests',testMatch:'**/*.spec.ts',workers:1,use:{baseURL:'http://127.0.0.1:1420',viewport:{width:1440,height:920},launchOptions:{executablePath:process.env.VIRTADE_CHROMIUM??(existsSync('/usr/bin/chromium')?'/usr/bin/chromium':undefined),args:['--no-sandbox']}},webServer:{command:'pnpm exec vite --mode test --host 127.0.0.1 --port 1420',url:'http://127.0.0.1:1420',reuseExistingServer:!process.env.CI},reporter:'list'});
