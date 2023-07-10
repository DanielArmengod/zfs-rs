import time
import click

@click.command()
@click.argument("file")
@click.argument("num_headers", type=int)
@click.argument("sleep_interval", type=float)
def cmd(file, num_headers, sleep_interval):
    f = open(file)
    l = f.readlines()
    print(''.join(l[:num_headers]), end='')
    for line in l[num_headers:]:
        print(line, end='')
        time.sleep(sleep_interval)

cmd()
