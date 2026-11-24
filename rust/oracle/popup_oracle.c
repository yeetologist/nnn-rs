/*
 * Differential oracle for rust/app/src/popup.rs: nnn.c's own parseargs, mkpath,
 * buffer_command_output, get_output, get_fs_info, show_help's text and the floating
 * window of show_content_in_floating_window (read back from curses' screen), against
 * rust/app/src/bin/oracle_popup.rs. Same line protocol as oracle.c: tab-separated fields,
 * strings in hex, one output line per call. Run by popup_run.sh.
 */
#define main nnn_main
#include "../../reference/nnn/src/nnn.c"
#undef main

static size_t unhex(const char *h, char *out)
{
	size_t n = 0;
	for (; h[0] && h[1]; h += 2)
		out[n++] = (char)((xchartohex((uchar_t)h[0]) << 4) | xchartohex((uchar_t)h[1]));
	out[n] = '\0';
	return n;
}

static void puthex(const char *s, size_t n)
{
	for (size_t i = 0; i < n; ++i)
		printf("%02x", (uchar_t)s[i]);
}

/* The text of row y, columns x..x+w-1, of what curses shows (curscr) */
static void putrow(int y, int x, int w)
{
	char out[4096];
	size_t n = 0;
	for (int i = 0; i < w; ++i) {
		cchar_t cc;
		wchar_t wch[CCHARW_MAX + 1] = {0};
		attr_t attrs;
		short pair;
		if (mvwin_wch(curscr, y, x + i, &cc) == ERR)
			continue;
		getcchar(&cc, wch, &attrs, &pair, NULL);
		if (attrs & A_ALTCHARSET) {
			switch (wch[0]) {
			case 'q': wch[0] = 0x2500; break;
			case 'x': wch[0] = 0x2502; break;
			case 'l': wch[0] = 0x250c; break;
			case 'k': wch[0] = 0x2510; break;
			case 'm': wch[0] = 0x2514; break;
			case 'j': wch[0] = 0x2518; break;
			}
		}
		for (int k = 0; k < CCHARW_MAX && wch[k]; ++k) {
			char mb[MB_LEN_MAX];
			int l = wctomb(mb, wch[k]);
			if (l > 0) {
				memcpy(out + n, mb, l);
				n += l;
			}
		}
		int cw = wcwidth(wch[0]);
		if (cw > 1)
			i += cw - 1;
	}
	puthex(out, n);
}

int main(void)
{
	static char line[1 << 17], a[1 << 16], b[1 << 15], c[1 << 15];
	char *f[10];

	setlocale(LC_COLLATE, "");
	setlocale(LC_CTYPE, "");
	home = "/home/u";
	pager = "cat";
	tmpfplen = (uchar_t)xstrsncpy(g_tmpfpath, "/tmp", TMP_LEN_MAX);
	pdents = xrealloc(pdents, total_dents * sizeof(struct entry));
	SCREEN *scr = newterm("xterm-256color", fopen("/dev/null", "w"), fopen("/dev/null", "r"));
	if (!scr)
		return 1;
	cbreak();
	noecho();
	keypad(stdscr, TRUE);
	refresh();

	while (fgets(line, sizeof line, stdin)) {
		line[strcspn(line, "\n")] = '\0';
		int nf = 0;
		for (char *p = strtok(line, "\t"); p && nf < 10; p = strtok(NULL, "\t"))
			f[nf++] = p;
		if (!nf)
			continue;
		const char *op = f[0];
		/* "-" stands for an empty field */
		for (int i = 1; i < nf; ++i)
			if (!strcmp(f[i], "-"))
				f[i] = "";
		if (!strcmp(op, "parseargs")) {
			char *argv[EXEC_ARGS_MAX] = {0};
			int idx = 0;
			unhex(f[1], a);
			char *cmd = parseargs(a, argv, &idx);
			if (!cmd)
				printf("NULL");
			else {
				printf("%d", idx);
				for (int i = 0; i < idx; ++i) {
					printf(" ");
					puthex(argv[i], strlen(argv[i]));
				}
				free(cmd);
			}
		} else if (!strcmp(op, "mkpath")) {
			char out[PATH_MAX];
			unhex(f[1], a);
			unhex(f[2], b);
			mkpath(a, b, out);
			puthex(out, strlen(out));
		} else if (!strcmp(op, "bufout")) {
			char *content = NULL;
			size_t len = 0;
			unhex(f[1], a);
			unhex(f[2], b);
			unhex(f[3], c);
			char *const cmds[] = {a, b};
			if (buffer_command_output(cmds, c[0] ? c : NULL, NULL, 2, &content, &len)) {
				puthex(content, len);
				free(content);
			} else
				printf("NULL");
		} else if (!strcmp(op, "getout")) {
			unhex(f[1], a);
			unhex(f[2], b);
			memset(g_buf, 0, sizeof(g_buf));
			if (get_output(a, b[0] ? b : NULL, NULL, -1, FALSE))
				puthex(g_buf, strlen(g_buf));
			else
				printf("NULL");
		} else if (!strcmp(op, "fsinfo")) {
			unhex(f[1], a);
			printf("%zu", get_fs_info(a, (uchar_t)atoi(f[2])));
		} else if (!strcmp(op, "help")) {
			/* help <curctx> <active mask> <hex path> <hex ctx paths, comma-separated> <hex session> */
			cfg.curctx = atoi(f[1]);
			int mask = atoi(f[2]);
			unhex(f[3], a);
			char *paths = f[4];
			for (int i = 0; i < CTX_MAX; ++i) {
				g_ctx[i].c_cfg.ctxactive = (mask >> i) & 1;
				char *comma = strchr(paths, ',');
				if (comma)
					*comma = '\0';
				unhex(paths, g_ctx[i].c_path);
				paths = comma ? comma + 1 : paths + strlen(paths);
			}
			unhex(f[5], curssn);
			/* catch what the pager (cat) prints, to print it as hex on one line */
			fflush(stdout);
			int saved = dup(STDOUT_FILENO);
			char outpath[] = "/tmp/nnn-phelp.XXXXXX";
			int ofd = mkstemp(outpath);
			dup2(ofd, STDOUT_FILENO);
			show_help(a);
			dup2(saved, STDOUT_FILENO);
			close(saved);
			lseek(ofd, 0, SEEK_SET);
			static char hb[1 << 16];
			ssize_t hn = read(ofd, hb, sizeof(hb));
			close(ofd);
			unlink(outpath);
			puthex(hb, hn > 0 ? (size_t)hn : 0);
		} else if (!strcmp(op, "float")) {
			/* float <lines> <cols> <perfile> <ndents> <cur> <hex name> <hex content> <keys> */
			xlines = (ushort_t)atoi(f[1]);
			xcols = (ushort_t)atoi(f[2]);
			resizeterm(xlines, xcols);
			clear();
			refresh();
			bool perfile = atoi(f[3]);
			ndents = atoi(f[4]);
			cur = atoi(f[5]);
			unhex(f[6], b);
			for (int i = 0; i < ndents; ++i)
				pdents[i].name = b;
			size_t len = unhex(f[7], a);
			/* keys, comma-separated; the original's loop ends on one that closes */
			int keys[64], nk = 0;
			for (char *p = strtok(f[8], ","); p && nk < 63; p = strtok(NULL, ","))
				keys[nk++] = atoi(p);
			keys[nk++] = 'q';
			for (int i = nk - 1; i >= 0; --i)
				keys[i] >= KEY_MIN ? ungetch(keys[i]) : unget_wch((wchar_t)keys[i]);
			int win_height = MIN(20, xlines - 4);
			int win_width = (xcols * 3) / 4;
			if (win_height < 5)
				win_height = 5;
			if (win_width < 20)
				win_width = 20;
			if (!show_content_in_floating_window(a, len, NULL, perfile)) {
				printf("NULL");
				flushinp();
			} else {
				int y0 = (xlines - win_height) / 2, x0 = (xcols - win_width) / 2;
				for (int r = 0; r < win_height; ++r) {
					if (r)
						printf(" ");
					putrow(y0 + r, x0, win_width);
				}
				flushinp();
			}
		}
		printf("\n");
		fflush(stdout);
	}
	endwin();
	return 0;
}
