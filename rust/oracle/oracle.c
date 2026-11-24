/*
 * Differential oracle: nnn.c's own internal functions, called on generated inputs.
 * Built with the default build's flags (see build.sh); main() is renamed so these
 * static functions can be reached from this translation unit.
 *
 * Protocol: one call per input line, fields separated by tabs, strings in hex.
 * One output line per call. The Rust oracle (rust/app/src/bin/oracle.rs) speaks
 * the same protocol; rust/oracle/run.sh compares the two.
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

static int sign(int v) { return (v > 0) - (v < 0); }

static void set_ic(int ic)
{
	fnstrstr = ic ? &strcasestr : &strstr;
	regflags = ic ? (REG_NOSUB | REG_EXTENDED | REG_ICASE) : (REG_NOSUB | REG_EXTENDED);
}

static void put_positions(const uchar_t *m)
{
	int first = 1;
	for (int i = 0; i < NAME_MAX; ++i)
		if (m[i]) {
			printf(first ? "%d" : ",%d", i);
			first = 0;
		}
}

static void list(const char *dir, const char *flags)
{
	cfg.showhidden = cfg.timeorder = cfg.sizeorder = cfg.extnorder = cfg.reverse = cfg.version = 0;
	cfg.blkorder = cfg.apparentsz = 0;
	cfg.timetype = T_MOD;
	for (const char *f = flags; *f; ++f)
		switch (*f) {
		case 'H': cfg.showhidden = 1; break;
		case 't': cfg.timeorder = 1; break;
		case 's': cfg.sizeorder = 1; break;
		case 'e': cfg.extnorder = 1; break;
		case 'r': cfg.reverse = 1; break;
		case 'v': cfg.version = 1; break;
		case 'A': cfg.timetype = T_ACCESS; break;
		case 'C': cfg.timetype = T_CHANGE; break;
		case 'd': cfg.blkorder = 1; break;
		case 'a': cfg.blkorder = cfg.apparentsz = 1; break;
		}
	entrycmpfn = cfg.reverse ? &reventrycmp : &entrycmp;
	namecmpfn = cfg.version ? &xstrverscasecmp : &xstricmp;

	char path[PATH_MAX];
	xstrsncpy(path, dir, PATH_MAX);
	int n = dentfill(path, &pdents);
	qsort(pdents, n, sizeof(*pdents), entrycmpfn);
	printf("%d", n);
	for (int i = 0; i < n; ++i) {
		printf(" ");
		puthex(pdents[i].name, strlen(pdents[i].name));
		printf(":%o:%lld:%lld:%u:%x:%u:%u", (unsigned)pdents[i].mode, (long long)pdents[i].size,
		       (long long)pdents[i].sec, pdents[i].nsec, (unsigned)(pdents[i].flags & ~FILE_YOUNG),
		       (unsigned)pdents[i].uid, (unsigned)pdents[i].gid);
		if (cfg.blkorder)
			printf(":%lld", (long long)pdents[i].blocks);
	}
	if (cfg.blkorder)
		printf(" |%llu:%lld", (unsigned long long)num_files, (long long)dir_blocks);
}

int main(void)
{
	static char line[1 << 16], a[1 << 15], b[1 << 15];
	char *f[8];

	/* As browse() does before the first dentfill() */
	pdents = xrealloc(pdents, total_dents * sizeof(struct entry));
	pnamebuf = (char *)xrealloc(pnamebuf, NAMEBUF_INCR);
	setlocale(LC_COLLATE, "");
	setlocale(LC_CTYPE, "");
	while (fgets(line, sizeof line, stdin)) {
		line[strcspn(line, "\n")] = '\0';
		int nf = 0;
		for (char *p = strtok(line, "\t"); p && nf < 8; p = strtok(NULL, "\t"))
			f[nf++] = p;
		if (!nf)
			continue;
		const char *op = f[0];
		if (!strcmp(op, "stricmp") || !strcmp(op, "verscmp")) {
			unhex(f[1], a); unhex(f[2], b);
			printf("%d", sign(op[0] == 's' ? xstricmp(a, b) : xstrverscasecmp(a, b)));
		} else if (!strcmp(op, "fuzzy") || !strcmp(op, "fscore") || !strcmp(op, "strstr")
			   || !strcmp(op, "fpos") || !strcmp(op, "spos") || !strcmp(op, "regex")) {
			set_ic(atoi(f[1]));
			unhex(f[2], a); unhex(f[3], b);
			uchar_t m[NAME_MAX];
			if (!strcmp(op, "fuzzy"))
				printf("%d", fuzzy_match(a, b));
			else if (!strcmp(op, "fscore"))
				printf("%d", fuzzy_match_score(a, b));
			else if (!strcmp(op, "strstr")) {
				fltrexp_t fe = { .str = a };
				printf("%d", visible_str(&fe, b));
			} else if (!strcmp(op, "fpos")) {
				fuzzy_match_positions(a, b, m);
				put_positions(m);
			} else if (!strcmp(op, "spos")) {
				string_match_positions(a, b, m);
				put_positions(m);
			} else {
				regex_t re;
				if (setfilter(&re, a))
					printf("bad");
				else {
					fltrexp_t fe = { .regex = &re };
					printf("%d", visible_re(&fe, b));
					regfree(&re);
				}
			}
		} else if (!strcmp(op, "coolsize")) {
			printf("%s", coolsize((off_t)strtoll(f[1], NULL, 10)));
		} else if (!strcmp(op, "perms")) {
			mode_t mode = (mode_t)strtoul(f[1], NULL, 10);
			printf("%s %c", get_lsperms(mode), get_detail_ind(mode));
		} else if (!strcmp(op, "unescape")) {
			unhex(f[1], a);
			wchar_t *w = unescape(a, (uint_t)atoi(f[2]));
			size_t n = wcstombs(b, w, sizeof b);
			if (n == (size_t)-1)
				printf("bad");
			else
				puthex(b, n);
		} else if (!strcmp(op, "time")) {
			time_t t = (time_t)strtoll(f[1], NULL, 10);
			struct tm tm;
			localtime_r(&t, &tm);
			printf("%s-%02d-%02d %02d:%02d", xitoa(tm.tm_year + 1900), tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min);
		} else if (!strcmp(op, "sortflags")) {
			cfg.timeorder = cfg.sizeorder = cfg.extnorder = cfg.reverse = cfg.version = 0;
			cfg.blkorder = cfg.apparentsz = cfg.showhidden = 0;
			cfg.timetype = T_MOD;
			entrycmpfn = &entrycmp;
			namecmpfn = &xstricmp;
			for (const char *k = f[1]; *k && strcmp(f[1], "-"); ++k) {
				int r = set_sort_flags(*k == '^' ? CONTROL('T') : *k);
				printf("%d,", r);
			}
			char s[8] = {0};
			int n = getorderstr(s);
			printf("[%.*s] %d%d%d%d%d", n + (n ? 1 : 0), s, cfg.timeorder, cfg.sizeorder, cfg.extnorder, cfg.reverse, cfg.version);
		} else if (!strcmp(op, "list")) {
			unhex(f[1], a);
			list(a, nf > 2 ? f[2] : "");
		} else
			printf("?");
		printf("\n");
		fflush(stdout);
	}
	return 0;
}
