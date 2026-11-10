import tailwindcss from '@tailwindcss/vite';
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';
import path from 'path';

export default defineConfig({
	resolve: {
		alias: {
			'$lib': path.resolve('./src/lib')
		}
	},
	server: {
		port: 5183,
		strictPort: true
	},
	plugins: [
		{
			name: 'ryotunes-dev-referrer-policy',
			configureServer(server) {
				server.middlewares.use((_req, res, next) => {
					res.setHeader('Referrer-Policy', 'no-referrer');
					next();
				});
			}
		},
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) => filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			// SPA fallback so the dynamic /playlist/[id] route resolves on direct load. desktop routing.
			adapter: adapter({ fallback: 'index.html' })
		})
	]
});
