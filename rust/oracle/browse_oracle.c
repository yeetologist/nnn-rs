/*
 * Differential oracle for browse.rs's pure functions: nnn.c's own is_text_file(),
 * xextension() and init_fcolors() (NNN_FCOLORS parsing; init_pair without a screen is a
 * no-op). Built like oracle.c (see build.sh flags).
 *
 * Protocol: one call per stdin line, "<fn>\t<arg in hex>", one output line per call:
 *   text <path>     -> 0|1
 *   ext <name>      -> the extension in hex, or NULL
 *   fcolors <env>   -> ret then fcolors[C_BLK..C_UND] in decimal
 * rust/app/src/bin/oracle_browse.rs speaks the same protocol; browse_run.py compares.
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

int main(void)
{
	static char line[1 << 16], arg[1 << 15];

	while (fgets(line, sizeof(line), stdin)) {
		line[strcspn(line, "\n")] = '\0';
		char *tab = strchr(line, '\t');
		if (!tab)
			continue;
		*tab = '\0';
		size_t n = unhex(tab + 1, arg);

		if (!strcmp(line, "text")) {
			printf("%d\n", is_text_file(arg));
		} else if (!strcmp(line, "ext")) {
			char *e = n ? xextension(arg, n) : NULL;
			if (!e)
				puts("NULL");
			else {
				for (; *e; ++e)
					printf("%02x", (uchar_t)*e);
				putchar('\n');
			}
		} else if (!strcmp(line, "fcolors")) {
			memset(fcolors, 0, sizeof(fcolors));
			if (n)
				setenv("NNN_FCOLORS", arg, 1);
			else
				unsetenv("NNN_FCOLORS");
			printf("%d", init_fcolors());
			for (int id = C_BLK; id <= C_UND; ++id)
				printf(" %u", fcolors[id]);
			putchar('\n');
		}
		fflush(stdout);
	}
	return 0;
}
