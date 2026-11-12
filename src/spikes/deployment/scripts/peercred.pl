# Unix-socket peer credential probe. A same-UID client connects and the server
# reports the kernel-provided SO_PEERCRED values. Written in Perl because the
# Deno FFI path for raw sockets crashes this runtime and Deno exposes no
# peer-credentials API.
use strict;
use warnings;
use Socket;

my ($path, $ready_path, $result_path) = @ARGV;
die "usage: peercred.pl <socket> <ready> <result>\n"
  unless defined $path && defined $ready_path && defined $result_path;

unlink $path;
socket(my $server, AF_UNIX, SOCK_STREAM, 0) or die "socket: $!";
bind($server, sockaddr_un($path)) or die "bind: $!";
listen($server, 1) or die "listen: $!";
chmod 0600, $path or die "chmod: $!";
my $mode = sprintf("%04o", (stat($path))[2] & 07777);

open(my $ready, ">", $ready_path) or die "ready: $!";
print $ready "{\"pid\":$$,\"socketMode\":\"$mode\"}\n";
close $ready;

my $connection;
accept($connection, $server) or die "accept: $!";
my $credential = getsockopt($connection, SOL_SOCKET, 17);
my ($peer_pid, $peer_uid, $peer_gid) = unpack("iII", $credential);

open(my $result, ">", $result_path) or die "result: $!";
print $result "{\"peerPid\":$peer_pid,\"peerUid\":$peer_uid,"
  . "\"peerGid\":$peer_gid,\"socketMode\":\"$mode\"}\n";
close $result;
