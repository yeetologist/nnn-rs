/*
 * Differential oracle for rust/app/src/session.rs: nnn.c's own save_session and
 * load_session against rust/app/src/bin/oracle_session.rs. Commands come from the file
 * argv[1], one per line, tab-separated, strings in hex ("-" empty); one output line per
 * command goes to argv[2]. Run by session_run.sh.
 *   save <cfg> <ndents> <hovered> <sname> then 8 x <c_cfg> <color> <name> <last> <fltr> <path>
 *        -> the session file's bytes in hex, or NOFILE
 *   load <cfg> <sname|~ for NULL> <restore> <picker> <file bytes>
 *        -> status and the state after the load (see dump())
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

static void puthex(FILE *o, const char *s, size_t n)
{
	for (size_t i = 0; i < n; ++i)
		fprintf(o, "%02x", (uchar_t)s[i]);
	if (!n)
		fputc('-', o);
}

static uint_t getbits(const settings *s)
{
	uint_t w;
	memcpy(&w, s, sizeof(w));
	return w;
}

static void setbits(settings *s, uint_t w)
{
	memcpy(s, &w, sizeof(w));
}

/* Reserved bits and a timetype of 3 have no Rust representation (see session.rs) */
static uint_t norm(uint_t w)
{
	w &= ~((1u << 12) | (1u << 20) | (3u << 22));
	if (((w >> 27) & 3) == 3)
		w &= ~(1u << 27);
	return w;
}

static char cfgdir[] = "/tmp/nnn-ssnc.XXXXXX";

/* Known state before each command: what a broken load leaves shows */
static void reset(uint_t cfgw)
{
	memset(g_ctx, 0, sizeof(g_ctx));
	for (int i = 0; i < CTX_MAX; ++i) {
		/* c_name and c_last stay empty: load_session clears the current context's by
		 * their first byte only, leaving bytes a short unterminated read shows again,
		 * which a Rust string cannot keep (see session.rs) */
		snprintf(g_ctx[i].c_path, PATH_MAX, "/old/path%d", i);
		g_ctx[i].c_fltr[0] = '/';
		snprintf(g_ctx[i].c_fltr + 1, REGEX_MAX - 1, "oldf%d", i);
		g_ctx[i].color = 100 + i;
		setbits(&g_ctx[i].c_cfg, 0x200u * (i & 1) | (uint_t)i);
	}
	setbits(&cfg, cfgw);
	memset(cfgsort, 0, sizeof(cfgsort));
	blk_shift = BLK_SHIFT_512;
	entrycmpfn = &entrycmp;
	namecmpfn = &xstricmp;
	curssn[0] = '\0';
	ndents = 0;
	cur = 0;
}

static void dump(FILE *o, bool st, const char *spath)
{
	fprintf(o, "%d %08x %d %d %u ", st, norm(getbits(&cfg)), entrycmpfn == &reventrycmp,
		namecmpfn == &xstrverscasecmp, blk_shift);
	puthex(o, (char *)cfgsort, sizeof(cfgsort));
	fputc(' ', o);
	puthex(o, curssn, strlen(curssn));
	for (int i = 0; i < CTX_MAX; ++i) {
		context *c = &g_ctx[i];
		fprintf(o, " %08x %u ", norm(getbits(&c->c_cfg)), c->color);
		/* strnlen: an unterminated string read from a broken file runs on into the
		 * next member in the original; the Rust side keeps the buffer's bytes only */
		puthex(o, c->c_name, strnlen(c->c_name, NAME_MAX + 1));
		fputc(',', o);
		puthex(o, c->c_last, strnlen(c->c_last, PATH_MAX));
		fputc(',', o);
		puthex(o, c->c_fltr, REGEX_MAX);
		fputc(',', o);
		puthex(o, c->c_path, strnlen(c->c_path, PATH_MAX));
	}
	fprintf(o, " %d", access(spath, F_OK) == 0);
}

int main(int argc, char **argv)
{
	static char line[1 << 21], a[1 << 20], spath[PATH_MAX];
	char *f[64];

	if (argc < 3)
		return 2;
	setlocale(LC_ALL, "");
	if (!mkdtemp(cfgdir))
		return 2;
	cfgpath = cfgdir;
	home = "/home/u";
	char sdir[PATH_MAX];
	snprintf(sdir, sizeof(sdir), "%s/sessions", cfgdir);
	mkdir(sdir, 0700);
	pdents = xrealloc(pdents, total_dents * sizeof(struct entry));
	SCREEN *scr = newterm("xterm-256color", fopen("/dev/null", "w"), fopen("/dev/null", "r"));
	if (!scr)
		return 1;
	xlines = 24;
	xcols = 80;
	FILE *in = fopen(argv[1], "r"), *o = fopen(argv[2], "w");
	if (!in || !o)
		return 2;

	while (fgets(line, sizeof line, in)) {
		line[strcspn(line, "\n")] = '\0';
		int nf = 0;
		for (char *p = strtok(line, "\t"); p && nf < 64; p = strtok(NULL, "\t"))
			f[nf++] = p;
		if (!nf)
			continue;
		for (int i = 1; i < nf; ++i)
			if (!strcmp(f[i], "-"))
				f[i] = "";
		if (!strcmp(f[0], "save") && nf >= 5 + 6 * CTX_MAX) {
			static char hov[NAME_MAX + 1], sname[NAME_MAX + 1];
			reset((uint_t)strtoul(f[1], NULL, 16));
			ndents = atoi(f[2]);
			unhex(f[3], hov);
			pdents[0].name = hov;
			unhex(f[4], sname);
			for (int i = 0; i < CTX_MAX; ++i) {
				char **g = f + 5 + 6 * i;
				context *c = &g_ctx[i];
				setbits(&c->c_cfg, (uint_t)strtoul(g[0], NULL, 16));
				c->color = (uint_t)strtoul(g[1], NULL, 10);
				unhex(g[2], c->c_name);
				unhex(g[3], c->c_last);
				memset(c->c_fltr, 0, REGEX_MAX);
				unhex(g[4], a);
				memcpy(c->c_fltr, a, REGEX_MAX);
				unhex(g[5], c->c_path);
			}
			snprintf(spath, sizeof(spath), "%s/%s", sdir, sname);
			unlink(spath);
			int presel = 0;
			save_session(sname, &presel);
			int fd = open(spath, O_RDONLY);
			if (fd == -1)
				fprintf(o, "NOFILE %d", presel);
			else {
				ssize_t n = read(fd, a, sizeof(a));
				close(fd);
				puthex(o, a, n > 0 ? (size_t)n : 0);
				fprintf(o, " %d", presel);
				unlink(spath);
			}
		} else if (!strcmp(f[0], "load") && nf >= 6) {
			static char sname[NAME_MAX + 1];
			char *path, *lastdir, *lastname;
			reset((uint_t)strtoul(f[1], NULL, 16));
			bool null = !strcmp(f[2], "~");
			if (!null)
				unhex(f[2], sname);
			bool restore = atoi(f[3]);
			g_state.picker = atoi(f[4]);
			snprintf(spath, sizeof(spath), "%s/%s", sdir, (null || restore) ? "@" : sname);
			if (strcmp(f[5], "NOFILE")) {
				size_t n = unhex(f[5], a);
				int fd = open(spath, O_CREAT | O_WRONLY | O_TRUNC, 0600);
				if (write(fd, a, n) != (ssize_t)n)
					return 3;
				close(fd);
			} else
				unlink(spath);
			bool st = load_session(null ? NULL : sname, &path, &lastdir, &lastname, restore);
			dump(o, st, spath);
			unlink(spath);
		}
		fputc('\n', o);
		fflush(o);
	}
	endwin();
	char cmd[PATH_MAX];
	snprintf(cmd, sizeof(cmd), "rm -rf %s", cfgdir);
	return system(cmd) == -1;
}
