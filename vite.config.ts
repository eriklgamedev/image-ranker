import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
export default defineConfig({plugins:[react()],server:{port:1420,strictPort:true},clearScreen:false,test:{include:['tests/**/*.test.ts','tests/**/*.test.tsx'],environment:'node'}});
