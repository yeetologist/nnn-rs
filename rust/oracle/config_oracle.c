/*
 * Differential oracle for the config/selection port (W4): nnn.c's own parsekvpair,
 * get_kv_val, get_kv_key, mkpath, xdirname, xbasename, common_prefix, xmktree and
 * write_lastdir, called on generated inputs. Same line protocol as oracle.c: tab-separated
 * fields, strings in hex ("-" for an empty string), one output line per call.
 * The Rust side is rust/app/src/bin/oracle_config.rs; run_config.sh compares them.
 * Paths of xmktree/write_lastdir calls are taken below $ORACLE_ROOT.
 */
#define main nnn_main
#include "../../reference/nnn/src/nnn.c"
#undef main
#include <sys/wait.h>

static size_t unhex(const char *h, char *out)
{
	size_t n = 0;
	if (strcmp(h, "-"))
		for (; h[0] && h[1]; h += 2)
			out[n++] = (char)((xchartohex((uchar_t)h[0]) << 4) | xchartohex((uchar_t)h[1]));
	out[n] = '\0';
	return n;
}

static void puthex(const char *s, size_t n)
{
	if (!n)
		printf("-");
	for (size_t i = 0; i < n; ++i)
		printf("%02x", (uchar_t)s[i]);
}

static char root[PATH_MAX];

/* root + "/" + rel into out */
static void rooted(const char *rel, char *out)
{
	snprintf(out, 1 << 15, "%s/%s", root, rel);
}

static void putkv(kv *arr, uchar_t max, const char *str)
{
	printf("%u", max);
	for (uchar_t i = 0; i < max; ++i) {
		printf(" %d:", arr[i].key);
		puthex(str + arr[i].off, strlen(str + arr[i].off));
	}
}

static void putfile(const char *path)
{
	char buf[1 << 14];
	int fd = open(path, O_RDONLY);
	if (fd == -1) {
		printf("nofile");
		return;
	}
	ssize_t n = read(fd, buf, sizeof buf);
	close(fd);
	puthex(buf, n > 0 ? (size_t)n : 0);
}

int main(void)
{
	static char line[1 << 17], a[1 << 15], b[1 << 15], c[1 << 15], d[1 << 15], e[1 << 15];
	char *f[8];
	const char *r = getenv("ORACLE_ROOT");

	xstrsncpy(root, r ? r : ".", PATH_MAX);
	while (fgets(line, sizeof line, stdin)) {
		line[strcspn(line, "\n")] = '\0';
		int nf = 0;
		for (char *p = strtok(line, "\t"); p && nf < 8; p = strtok(NULL, "\t"))
			f[nf++] = p;
		if (!nf)
			continue;
		const char *op = f[0];
		if (!strcmp(op, "kv") || !strcmp(op, "kvval") || !strcmp(op, "kvkey")) {
			unhex(f[1], a);
			uchar_t id = !strcmp(op, "kvkey") ? NNN_ORDER
				     : (!strcmp(op, "kvval") && f[3][0] == 'P') ? NNN_PLUG : NNN_BMS;
			kv *arr = NULL;
			char *str = NULL;
			uchar_t max = 0;
			setenv(env_cfg[id], a, 1);
			bool ok = parsekvpair(&arr, &str, id, &max);
			if (!ok)
				printf("bad");
			else if (!strcmp(op, "kv"))
				putkv(arr, max, str);
			else if (!strcmp(op, "kvval")) {
				if (id == NNN_PLUG)
					pluginstr = str;
				else
					bmstr = str;
				unhex(f[4], c);
				home = c;
				char *v = get_kv_val(arr, NULL, atoi(f[2]), max, id);
				if (v)
					puthex(v, strlen(v));
				else
					printf("null");
			} else {
				orderstr = str;
				unhex(f[2], b);
				printf("%d", get_kv_key(arr, b, max, id));
			}
		} else if (!strcmp(op, "mkpath")) {
			static char out[PATH_MAX];
			unhex(f[1], a); unhex(f[2], b); unhex(f[3], c);
			home = c;
			memset(out, 0, sizeof out);
			size_t n = mkpath(a, b, out);
			puthex(out, strnlen(out, PATH_MAX));
			printf(":%zu", n);
		} else if (!strcmp(op, "dirname") || !strcmp(op, "basename")) {
			unhex(f[1], a);
			char *s = op[0] == 'd' ? xdirname(a) : xbasename(a);
			puthex(s, strlen(s));
		} else if (!strcmp(op, "cprefix")) {
			unhex(f[1], a); unhex(f[2], b);
			char *s = common_prefix(a, b);
			if (s)
				puthex(s, strlen(s));
			else
				printf("null");
		} else if (!strcmp(op, "mktree")) {
			unhex(f[1], b);
			rooted(b, a);
			printf("%d", xmktree(a, f[2][0] == '1', f[3][0] == '1'));
		} else if (!strcmp(op, "lastdir")) {
			/* curpath, outfile ("~..." kept, else rooted; "-" for none), cfgpath, home */
			unhex(f[1], a);
			unhex(f[2], b);
			int none = !strcmp(f[2], "-");
			if (!none && !tilde_is_home(b)) {
				rooted(b, e);
				xstrsncpy(b, e, sizeof b);
			}
			unhex(f[3], e); rooted(e, c);
			unhex(f[4], e); rooted(e, d);
			cfgpath = c;
			home = d;
			fflush(stdout);
			pid_t pid = fork();
			if (pid == 0) {
				/* exit() in printerr() would rewind the shared stdin offset */
				close(STDIN_FILENO);
				write_lastdir(a, none ? NULL : b);
				_exit(0);
			}
			int st;
			waitpid(pid, &st, 0);
			printf("%s ", WIFEXITED(st) && WEXITSTATUS(st) == 0 ? "ok" : "err");
			if (none)
				mkpath(cfgpath, ".lastd", e);
			else if (!convert_tilde(b, e))
				xstrsncpy(e, b, sizeof e);
			putfile(e);
		} else
			printf("?");
		printf("\n");
		fflush(stdout);
	}
	return 0;
}
