// Small offline ray-query accelerator. No model/file access; owns copied triangles.
#include <algorithm>
#include <array>
#include <atomic>
#include <cmath>
#include <thread>
#include <vector>
struct V { double x,y,z;double operator[](int i)const{return i==0?x:i==1?y:z;} };
static V sub(V a,V b){return {a.x-b.x,a.y-b.y,a.z-b.z};}
static V cross(V a,V b){return {a.y*b.z-a.z*b.y,a.z*b.x-a.x*b.z,a.x*b.y-a.y*b.x};}
static double dot(V a,V b){return a.x*b.x+a.y*b.y+a.z*b.z;}
struct Tri {V a,e1,e2,lo,hi,c;};
struct Node {V lo,hi;int start,count,left=-1,right=-1;};
struct Scene {
 std::vector<Tri> tris;std::vector<int> ids;std::vector<Node> nodes;
 int build(int start,int count){
  Node n; n.start=start;n.count=count;n.lo={INFINITY,INFINITY,INFINITY};n.hi={-INFINITY,-INFINITY,-INFINITY};
  for(int k=start;k<start+count;k++){const auto&t=tris[ids[k]];n.lo={std::min(n.lo.x,t.lo.x),std::min(n.lo.y,t.lo.y),std::min(n.lo.z,t.lo.z)};n.hi={std::max(n.hi.x,t.hi.x),std::max(n.hi.y,t.hi.y),std::max(n.hi.z,t.hi.z)};}
  int at=nodes.size();nodes.push_back(n);
  if(count>8){V d=sub(n.hi,n.lo);int axis=d.x>d.y?(d.x>d.z?0:2):(d.y>d.z?1:2);int mid=start+count/2;
   std::nth_element(ids.begin()+start,ids.begin()+mid,ids.begin()+start+count,[&](int a,int b){return tris[a].c[axis]<tris[b].c[axis];});
   nodes[at].left=build(start,count/2);nodes[at].right=build(mid,count-count/2);
  }return at;
 }
 bool box(const Node&n,V o,V d,double limit)const{
  double lo=0,hi=limit;for(int k=0;k<3;k++){
   if(std::abs(d[k])<1e-14){if(o[k]<n.lo[k]||o[k]>n.hi[k])return false;continue;}
   double a=(n.lo[k]-o[k])/d[k],b=(n.hi[k]-o[k])/d[k];if(a>b)std::swap(a,b);lo=std::max(lo,a);hi=std::min(hi,b);if(hi<lo)return false;
  }return true;
 }
 void ray(int node,V o,V d,double&best,long long&hit)const{
  const auto&n=nodes[node];if(!box(n,o,d,best))return;
  if(n.left>=0){ray(n.left,o,d,best,hit);ray(n.right,o,d,best,hit);return;}
  for(int k=n.start;k<n.start+n.count;k++){int id=ids[k];const auto&t=tris[id];V p=cross(d,t.e2);double det=dot(t.e1,p);if(std::abs(det)<1e-10)continue;double inv=1/det;V tv=sub(o,t.a);double u=dot(tv,p)*inv;if(u< -1e-8||u>1+1e-8)continue;V q=cross(tv,t.e1);double v=dot(d,q)*inv;if(v< -1e-8||u+v>1+1e-8)continue;double dist=dot(t.e2,q)*inv;if(dist>1e-4&&dist<best){best=dist;hit=id;}}
 }
 int crossings(int node,V o,V d)const{
  const auto&n=nodes[node];if(!box(n,o,d,INFINITY))return 0;
  if(n.left>=0)return crossings(n.left,o,d)+crossings(n.right,o,d);
  int total=0;for(int k=n.start;k<n.start+n.count;k++){
   const auto&t=tris[ids[k]];V h=cross(d,t.e2);double det=dot(t.e1,h);if(std::abs(det)<=1e-9)continue;
   double inv=1/det;V s=sub(o,t.a);double u=dot(s,h)*inv;V q=cross(s,t.e1);double v=dot(d,q)*inv;double dist=dot(t.e2,q)*inv;
   if(u>=0&&v>=0&&u+v<=1&&dist>1e-6)total++;
  }return total;
 }
};
#if defined(_WIN32)
#define CAD_EXPORT __declspec(dllexport)
#else
#define CAD_EXPORT __attribute__((visibility("default")))
#endif
extern "C" {
// Orthographic z-buffer: retain only visible fragments, not the enormous
// Python arrays of every overlapping triangle's bounding-box candidates.
CAD_EXPORT void cad_rasterize(const double*p,int count,int width,int height,double*depth,long long*ids){
 struct Screen {double ax,ay,az,bx,by,bz,cx,cy,cz,area;int x0,x1,y0,y1,id;};
 std::vector<Screen> tris;tris.reserve(count);
 for(int i=0;i<count;i++){
  const double*t=p+i*9;double area=(t[3]-t[0])*(t[7]-t[1])-(t[6]-t[0])*(t[4]-t[1]);
  if(!std::isfinite(area)||std::abs(area)<=1e-12)continue;
  int x0=std::max(0,(int)std::ceil(std::min({t[0],t[3],t[6]})-.5)),x1=std::min(width-1,(int)std::floor(std::max({t[0],t[3],t[6]})-.5));
  int y0=std::max(0,(int)std::ceil(std::min({t[1],t[4],t[7]})-.5)),y1=std::min(height-1,(int)std::floor(std::max({t[1],t[4],t[7]})-.5));
  if(x1>=x0&&y1>=y0)tris.push_back({t[0],t[1],t[2],t[3],t[4],t[5],t[6],t[7],t[8],area,x0,x1,y0,y1,i});
 }
 std::fill(depth,depth+(size_t)width*height,INFINITY);std::fill(ids,ids+(size_t)width*height,-1);
 std::atomic<int> row{0};auto work=[&](){for(;;){int begin=row.fetch_add(64);if(begin>=height)return;int end=std::min(begin+64,height);
  for(const auto&t:tris){if(t.y1<begin||t.y0>=end)continue;double inv=1/t.area;
   for(int y=std::max(begin,t.y0);y<=std::min(end-1,t.y1);y++)for(int x=t.x0;x<=t.x1;x++){
    double px=x+.5,py=y+.5;
    double a=((t.bx-px)*(t.cy-py)-(t.cx-px)*(t.by-py))*inv;
    double b=((t.cx-px)*(t.ay-py)-(t.ax-px)*(t.cy-py))*inv,c=1-a-b;
    if(a< -1e-7||b< -1e-7||c< -1e-7)continue;
    double z=a*t.az+b*t.bz+c*t.cz;size_t at=(size_t)y*width+x;
    if(z<=depth[at]){depth[at]=z;ids[at]=t.id;}
   }
  }
 }};
 unsigned threads=std::min(8u,std::max(1u,std::thread::hardware_concurrency()));std::vector<std::thread>pool;
 for(unsigned i=1;i<threads;i++)pool.emplace_back(work);work();for(auto&t:pool)t.join();
}
CAD_EXPORT void* cad_rays_build(const double* p,int count){try{if(count<1)return nullptr;auto*s=new Scene;s->tris.reserve(count);for(int i=0;i<count;i++){
 V a{p[9*i],p[9*i+1],p[9*i+2]},b{p[9*i+3],p[9*i+4],p[9*i+5]},c{p[9*i+6],p[9*i+7],p[9*i+8]};
 s->tris.push_back({a,sub(b,a),sub(c,a),{std::min({a.x,b.x,c.x}),std::min({a.y,b.y,c.y}),std::min({a.z,b.z,c.z})},{std::max({a.x,b.x,c.x}),std::max({a.y,b.y,c.y}),std::max({a.z,b.z,c.z})},{(a.x+b.x+c.x)/3,(a.y+b.y+c.y)/3,(a.z+b.z+c.z)/3}});s->ids.push_back(i);
 }s->nodes.reserve(count*2);s->build(0,count);return s;}catch(...){return nullptr;}}
CAD_EXPORT void cad_rays_free(void* s){delete static_cast<Scene*>(s);}
CAD_EXPORT void cad_rays_inside(void* ptr,const double*points,int count,const double*directions,int rays,unsigned char*inside){
 auto*s=static_cast<Scene*>(ptr);for(int i=0;i<count;i++){
  V p{points[i*3],points[i*3+1],points[i*3+2]};int votes=0;
  for(int j=0;j<rays;j++)votes+=s->crossings(0,p,{directions[j*3],directions[j*3+1],directions[j*3+2]})%2;
  inside[i]=votes>rays/2;
 }
}
CAD_EXPORT void cad_rays_cast(void* ptr,const double*o,const double*d,int count,double*best,long long*hit){
 auto*s=static_cast<Scene*>(ptr);std::atomic<int> next{0};auto work=[&](){for(;;){int start=next.fetch_add(256);if(start>=count)return;for(int i=start;i<std::min(start+256,count);i++){hit[i]=-1;s->ray(0,{o[i*3],o[i*3+1],o[i*3+2]},{d[i*3],d[i*3+1],d[i*3+2]},best[i],hit[i]);}}};
 unsigned threads=std::min(8u,std::max(1u,std::thread::hardware_concurrency()));if(count<2048)threads=1;std::vector<std::thread>pool;for(unsigned i=1;i<threads;i++)pool.emplace_back(work);work();for(auto&t:pool)t.join();
}
}
