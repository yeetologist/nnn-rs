/*
 * Differential oracle for rust/app/src/fileops.rs: entries_in_file, opstr and the
 * P_CPMVFMT/P_CPMVRNM patterns, called through nnn.c itself. Same flags as build.sh; the
 * Rust side is rust/app/src/bin/oracle_fileops.rs.
 *
 * Protocol, one op per line, tab-separated, strings in hex; one output line per op:
 *   E <buflen> <delim> <hex>   entries_in_file on a file holding <hex>  -> E <count>
 *   O <hexop> <hexselpath>     opstr                                    -> O <hex>
 *   F <hextmp>                 patterns[P_CPMVFMT]                      -> F <hex>
 *   N <hexesc> <hextmp> <hexcmd> patterns[P_CPMVRNM]                    -> N <hex>
 */
#define main nnn_main
#include "../../reference/nnn/src/nnn.c"
#undef main

static size_t unhex(const char *h, char *out)
{
	size_t n = 0;
	for (; h[0] && h[1] && h[0] != '\t'; h += 2)
		out[n++] = (char)((xchartohex((uchar_t)h[0]) << 4) | xchartohex((uchar_t)h[1]));
	out[n] = '\0';
	return n;
}

static void puthex(const char *s, size_t n)
{
	for (size_t i = 0; i < n; ++i)
		printf("%02x", (uchar_t)s[i]);
}

static const char *field(const char *s, int i)
{
	while (i--) {
		s = strchr(s, '\t');
		if (!s)
			return "";
		++s;
	}
	return s;
}

int main(void)
{
	static char line[1 << 16], a1[1 << 15], a2[1 << 15], a3[1 << 15], out[1 << 16];
	while (fgets(line, sizeof(line), stdin)) {
		line[strcspn(line, "\n")] = '\0';
		char op = line[0];
		printf("%c ", op);
		switch (op) {
		case 'E': {
			size_t buflen = (size_t)atoi(field(line, 1));
			char delim = (char)atoi(field(line, 2));
			size_t n = unhex(field(line, 3), a1);
			char tmpl[] = "/tmp/nnn-fo-oracle.XXXXXX";
			int fd = mkstemp(tmpl);
			if (write(fd, a1, n) != (ssize_t)n)
				return 2;
			lseek(fd, 0, SEEK_SET);
			char *buf = malloc(buflen);
			printf("%u", entries_in_file(fd, buf, buflen, delim));
			free(buf);
			close(fd);
			unlink(tmpl);
			break;
		}
		case 'O':
			unhex(field(line, 1), a1);
			unhex(field(line, 2), a2);
			selpath = a2;
			opstr(out, a1);
			puthex(out, strlen(out));
			break;
		case 'F':
			unhex(field(line, 1), a1);
			snprintf(out, sizeof(out), patterns[P_CPMVFMT], a1);
			puthex(out, strlen(out));
			break;
		case 'N':
			unhex(field(line, 1), a1);
			unhex(field(line, 2), a2);
			unhex(field(line, 3), a3);
			snprintf(out, sizeof(out), patterns[P_CPMVRNM], a1, a2, a3);
			puthex(out, strlen(out));
			break;
		}
		putchar('\n');
		fflush(stdout);
	}
	return 0;
}
